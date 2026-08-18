//! Order pool

use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::{crypto::RecoveryError, transaction::TxHashRef},
		primitives::{B256, TxHash},
	},
	dashmap::{DashMap, DashSet, Entry},
	parking_lot::RwLock,
	reth::primitives::{Recovered, SealedHeader},
	std::{sync::Arc, time::Instant},
};

mod host;
mod lifecycle;
mod maintain;
mod native;
mod report;
mod rpc;
mod select;
mod setup;
mod step;

// Order Pool public API
pub use {
	lifecycle::{TxLifecycle, TxLifecycleLog, TxMined, TxPayloadStage, TxSource},
	rpc::{BundleResult, BundlesApiClient},
	setup::HostNodeInstaller,
	step::{
		AppendOrders,
		OrderInclusionAttempt,
		OrderInclusionFailure,
		OrderInclusionSuccess,
	},
};

#[derive(Debug, Clone)]
pub enum Order<P: Platform> {
	/// A single transaction.
	Transaction(Recovered<types::Transaction<P>>),
	/// A bundle of transactions.
	Bundle(types::Bundle<P>),
}

impl<P: Platform> Order<P> {
	pub fn hash(&self) -> B256 {
		match self {
			Order::Transaction(tx) => *tx.tx_hash(),
			Order::Bundle(bundle) => bundle.hash(),
		}
	}

	pub fn transactions(&self) -> &[Recovered<types::Transaction<P>>] {
		match self {
			Order::Bundle(bundle) => bundle.transactions(),
			Order::Transaction(tx) => core::slice::from_ref(tx),
		}
	}

	pub fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		match self {
			Order::Transaction(tx) => tx.try_into_executable(),
			Order::Bundle(bundle) => bundle.try_into_executable(),
		}
	}

	pub const fn is_bundle(&self) -> bool {
		matches!(self, Order::Bundle(_))
	}
}

/// Implements an order pool that handles mempool operations for transactions
/// and bundles.
///
/// Notes:
///  - This type is cheap to clone, all clones of this type share the same
///    underlying instance.
///  - This type is referenced by steps and RPC modules when constructing a
///    pipeline and Reth node.
pub struct OrderPool<P: Platform> {
	inner: Arc<OrderPoolInner<P>>,
}

/// Orders manipulation
impl<P: Platform> OrderPool<P> {
	/// Adds a new order to the pool and makes it potentially available to be
	/// returned by `best_orders()`.
	pub fn insert(&self, order: Order<P>) {
		let order_hash = order.hash();
		let received = Instant::now();
		let source = match &order {
			Order::Transaction(_) => TxSource::Transaction,
			Order::Bundle(_) => TxSource::Bundle(order_hash),
		};

		for tx in order.transactions() {
			// keep track of all orders that contain this transaction.
			// When this transaction ends up in a produced payload, all orders
			// containing it will be invalidated and removed from the pool.
			let tx_hash = *tx.tx_hash();
			self
				.inner
				.tx_map
				.entry(tx_hash)
				.or_default()
				.insert(order_hash);

			// start tracking the lifecycle of this transaction
			self
				.inner
				.lifecycle
				.record_received(tx_hash, source, received);
		}

		// remember the members of the bundle so that pipeline events keyed by
		// its hash can be resolved after it left the pool. A transaction order
		// is keyed by its own transaction hash and needs no record.
		if order.is_bundle() {
			let members = order
				.transactions()
				.iter()
				.map(|tx| *tx.tx_hash())
				.collect();
			self
				.inner
				.lifecycle
				.record_order(order_hash, members, received);
		}

		self.inner.orders.insert(order_hash, order);
	}

	/// Removes an order from the pool and makes it no longer available through
	/// `best_orders()`. This is the single eviction path of the pool: every
	/// member transaction of the order is unmapped from it, and a member that
	/// no other live order holds is marked as dropped in the lifecycle log,
	/// unless it was received from the host node mempool, where it still lives.
	pub fn remove(&self, order_hash: &B256) {
		let removed = self.inner.orders.remove(order_hash);
		self.inner.host.remove_transaction(*order_hash);

		if let Some((_, order)) = removed {
			let now = Instant::now();
			// `release_member` returns its `tx_map` guard before the lifecycle
			// log is called for the orphaned member.
			order
				.transactions()
				.iter()
				.map(|tx| *tx.tx_hash())
				.filter(|tx_hash| !self.release_member(tx_hash, order_hash))
				.for_each(|tx_hash| {
					self
						.inner
						.lifecycle
						.record_dropped_unless_mempool(tx_hash, now);
				});
		}
	}

	/// Removes all orders that contain a specific transaction hash. The
	/// transaction itself is also removed from the transaction pool of the host
	/// node, so it is marked as dropped in the lifecycle log whatever its
	/// source (a no-op if it was mined); the orders holding it are removed
	/// through [`Self::remove`], which takes care of their other members.
	pub fn remove_any_with(&self, tx_hash: TxHash) {
		self.inner.host.remove_transaction(tx_hash);
		self.inner.lifecycle.record_dropped_now(tx_hash);

		// take the set out of the map before removing the orders, so that no
		// guard on `tx_map` is held while `remove` updates it.
		let orders = self.inner.tx_map.remove(&tx_hash);
		orders
			.into_iter()
			.flat_map(|(_, orders)| orders)
			.for_each(|order_hash| self.remove(&order_hash));
	}

	/// Unmaps `order_hash` from the set of orders holding `tx_hash`, dropping
	/// the map entry once the set is empty. Returns `true` if another live
	/// order still holds the transaction.
	fn release_member(&self, tx_hash: &TxHash, order_hash: &B256) -> bool {
		match self.inner.tx_map.entry(*tx_hash) {
			Entry::Occupied(occupied) => {
				occupied.get().remove(order_hash);
				let still_live = !occupied.get().is_empty();
				if !still_live {
					occupied.remove();
				}
				still_live
			}
			Entry::Vacant(_) => false,
		}
	}
}

impl<P: Platform> OrderPool<P> {
	/// Returns true if the order pool knows for sure that the bundle will never
	/// be eligible for inclusion in any block. This check requires the order pool
	/// to be attached to a host Reth node, otherwise it always returns false.
	pub fn is_permanently_ineligible(&self, bundle: &types::Bundle<P>) -> bool {
		self
			.inner
			.host
			.map_tip_header(|header| bundle.is_permanently_ineligible(header))
			.unwrap_or(false)
	}

	/// Returns `true` if the order pool is attached to a host Reth node.
	/// This is done by the `attach_pool` method during node components setup.
	pub fn is_attached_to_host(&self) -> bool {
		self.inner.host.is_attached()
	}
}

/// Transaction lifecycle
impl<P: Platform> OrderPool<P> {
	/// Creates an order pool that records the lifecycle of the transactions it
	/// sees into the given log. Use this to configure the retention and
	/// capacity of the log, otherwise `OrderPool::default()` creates a log with
	/// default settings.
	pub fn with_lifecycle(lifecycle: TxLifecycleLog) -> Self {
		Self {
			inner: Arc::new(OrderPoolInner {
				lifecycle,
				..Default::default()
			}),
		}
	}

	/// Returns the log that records when transactions seen by this pool were
	/// received, considered for inclusion, included in a payload and mined.
	pub fn lifecycle(&self) -> &TxLifecycleLog {
		&self.inner.lifecycle
	}
}

impl<P: Platform> Clone for OrderPool<P> {
	fn clone(&self) -> Self {
		Self {
			inner: Arc::clone(&self.inner),
		}
	}
}

impl<P: Platform> Default for OrderPool<P> {
	fn default() -> Self {
		Self {
			inner: Arc::new(OrderPoolInner::default()),
		}
	}
}

#[derive(Default)]
struct OrderPoolInner<P: Platform> {
	orders: DashMap<B256, Order<P>>,

	/// A map that keeps track of transaction hashes and orders that contain
	/// them.
	tx_map: DashMap<TxHash, DashSet<B256>>,

	/// The host Reth node that this order pool is attached to.
	/// Attachment is done by the `attach_pool` method during node components
	host: Arc<host::HostNode<P>>,

	/// Records when transactions seen by this pool were received, considered
	/// for inclusion, included in a payload and mined.
	lifecycle: TxLifecycleLog,
}

impl<P: Platform> OrderPoolInner<P> {
	pub(crate) fn outer(self: &Arc<Self>) -> OrderPool<P> {
		OrderPool {
			inner: Arc::clone(self),
		}
	}
}

impl<P: Platform> From<Arc<OrderPoolInner<P>>> for OrderPool<P> {
	fn from(inner: Arc<OrderPoolInner<P>>) -> Self {
		inner.outer()
	}
}

#[cfg(feature = "test-utils")]
pub(crate) use native::NativeTransactionPool;
