//! Order pool

use {
	crate::alloy::{
		consensus::crypto::RecoveryError,
		primitives::{TxHash, B256},
	},
	crate::pool::pending::PendingOrders,
	crate::{prelude::*, reth},
	dashmap::{DashMap, DashSet},
	parking_lot::Mutex,
	reth::{
		ethereum::primitives::SignedTransaction,
		primitives::{Recovered, SealedHeader},
	},
	std::sync::Arc,
};

mod host;
mod maintain;
mod pending;
mod report;
mod rpc;
mod select;
mod setup;
mod step;

// Order Pool public API
pub use {
	rpc::{BundleResult, BundlesApiClient},
	setup::HostNodeInstaller,
	step::{
		AppendOrders, OrderInclusionAttempt, OrderInclusionFailure,
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
		let mut pending = self.inner.pending.lock();
		// first evaluate with the pending ordes,
		// this may cause other orders to become valid as well
		pending.push(order);

		while let Some(order) = pending.pop() {
			let valid_hash = order.hash();
			for tx in order.transactions() {
				// keep track of all orders that contain this transaction.
				// When this transaction ends up in a produced payload, all orders
				// containing it will be invalidated and removed from the pool.
				let txhash = *tx.tx_hash();
				self
					.inner
					.txmap
					.entry(txhash)
					.or_default()
					.insert(valid_hash);
			}
			self.inner.orders.insert(valid_hash, order);
		}
	}

	/// Removes an order from the pool and makes it no longer available through
	/// `best_orders()`.
	pub fn remove(&self, order_hash: &B256) -> Option<Order<P>> {
		self.inner.orders.remove(order_hash).map(|(_, order)| order)
	}

	/// Removes all orders that contain the a specific transaction hash.
	pub fn remove_any_with(&self, txhash: TxHash) {
		if let Some((_, orders)) = self.inner.txmap.remove(&txhash) {
			for order_hash in orders {
				self.remove(&order_hash);
			}
		}
	}

	/// Returns true if the order pool knows for sure that the bundle will never
	/// be eligible for inclusion in any block.
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

	pub fn best_orders_for_block<'a>(
		&'a self,
		block: &'a BlockContext<P>,
	) -> impl Iterator<Item = Order<P>> + 'a {
		self
			.inner
			.orders
			.iter()
			.filter_map(|entry| match entry.value() {
				t @ Order::Transaction(_) => Some(t.clone()),
				b @ Order::Bundle(bundle) => {
					bundle.is_eligible(block).then(|| b.clone())
				}
			})
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
	/// Orders that are not yet known to be vaild, but could be in the future
	pending: Mutex<PendingOrders<P>>,

	/// Orders that are guaranteed to be valid
	orders: DashMap<B256, Order<P>>,

	/// A map that keeps track of transaction hashes and orders that contain
	/// them.
	txmap: DashMap<TxHash, DashSet<B256>>,

	/// The host Reth node that this order pool is attached to.
	/// Attachment is done by the `attach_pool` method during node components
	host: Arc<host::HostNode<P>>,
}

impl<P: Platform> OrderPoolInner<P> {
	pub fn outer(self: &Arc<Self>) -> OrderPool<P> {
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
