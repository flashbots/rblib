//! Order pool

use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::crypto::RecoveryError,
		primitives::{B256, TxHash},
	},
	dashmap::{DashMap, DashSet},
	parking_lot::RwLock,
	reth::{
		ethereum::primitives::SignedTransaction,
		primitives::{Recovered, SealedHeader},
	},
	std::sync::Arc,
};

mod host;
mod maintain;
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

type NonceBitmap = u64;
const BITMAP_BITS: u64 = core::mem::size_of::<NonceBitmap>() as u64 * 8;

#[derive(Debug, Default)]
struct KnownNonces {
	// Next nonce known to be valid
	expected: u64,
	/// Bitmap of relative seen nonces
	seen_relative: NonceBitmap,
}

/// Implements a holding place for orders arriving in the wrong order
#[derive(Default, Debug)]
struct PendingOrders<P: Platform> {
	nonces: HashMap<Address, KnownNonces>,
	orders: HashMap<B256, Order<P>>,
	blocked: HashMap<Address, B256>,
	outbox: VecDeque<Order<P>>,
}

enum Pending {
	Valid,
	Retained,
	Invalid,
}

impl<P: Platform> PendingOrders<P> {
	fn process(&mut self, order: Order<P>) -> Pending {
		let mut unresolved = HashSet::new();
		let mut updated = HashSet::new();

		for transaction in order.transactions() {
			let addr = transaction.signer();
			let nonce = transaction.nonce();

			println!("transaction: {addr} {nonce}");

			// TODO: get the expected nonce from stateroot
			let KnownNonces {
				expected,
				seen_relative,
			} = self.nonces.entry(addr).or_default();

			if nonce < *expected {
				// repeated nonce, always invalid
				return Pending::Invalid;
			} else if nonce == *expected {
				println!("e");
				// how many consecutive nonces have we have already seen from this address?
				let filled = 1 + seen_relative.trailing_ones() as u64;
				println!("filled: {filled} nonce: {nonce}");
				*expected = nonce + filled;
				*seen_relative >>= filled;
			} else {
				println!("f");
				let rel = nonce - *expected - 1;
				*seen_relative |= 1 << rel
			}

			updated.insert(addr);
			println!("after {seen_relative:#b}");

			// did we resolve the last relative_seen?
			if *seen_relative == 0 {
				println!("g");
				unresolved.remove(&addr);
			} else {
				println!("h");
				unresolved.insert(addr);
			}

			println!("nonces {:?}", self.nonces);
		}

		if unresolved.is_empty() {
			self.outbox.push_back(order);

			// recurse on dependent orders
			for addr in updated.iter() {
				self.blocked.remove(addr).map(|hash| {
					self.orders.remove(&hash).map(|order| {
						println!("re-process {self:?}");
						self.process(order)
					})
				});
			}

			Pending::Valid
		} else {
			let order_hash = order.hash();
			for addr in unresolved.drain() {
				self.blocked.insert(addr, order_hash);
			}
			self.orders.insert(order_hash, order);
			Pending::Retained
		}
	}

	fn take(&mut self) -> Option<Order<P>> {
		self.outbox.pop_front()
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
		match pending.process(order) {
			Pending::Valid => {
				while let Some(order) = pending.take() {
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
			Pending::Retained => (),
			Pending::Invalid => (),
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

#[cfg(test)]
mod test {
	use {
		super::{FlashbotsBundle, Order, OrderPool},
		crate::prelude::{BlockContext, Optimism},
		crate::test_utils::PrivateKeySignerOpExt,
		crate::test_utils::*,
	};

	fn mock_block() -> BlockContext<Optimism> {
		BlockContext::mocked().0
	}

	#[test]
	fn test_pool_iterator_valid() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(1));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 1);
	}

	#[test]
	fn test_pool_iterator_invalid() {
		let op = OrderPool::default();

		// two transactions with the same nonce!
		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(0));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 0);
	}

	#[test]
	fn test_pool_iterator_single_bundle_reverse() {
		let op = OrderPool::default();

		// two transactions in reverse order

		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2))
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 1);
	}

	#[test]
	fn test_pool_iterator_single_bundle_alternating() {
		let op = OrderPool::default();

		// two transactions in reverse order

		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 1);
	}

	#[test]
	fn test_pool_iterator_invalid_nonce_gap() {
		let op = OrderPool::default();

		// two transactions in reverse order
		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 0);
	}

	#[test]
	fn test_pool_iterator_multiple_correct_order_a() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(1));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(2))
			.with_transaction(account.test_tx_with_nonce(3));

		op.insert(Order::Bundle(bundle_a));
		op.insert(Order::Bundle(bundle_b));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}

	#[test]
	fn test_pool_iterator_multiple_correct_order_b() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle_a));
		op.insert(Order::Bundle(bundle_b));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}

	#[test]
	fn test_pool_iterator_multiple_reverse_order_a() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(1));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(2))
			.with_transaction(account.test_tx_with_nonce(3));

		op.insert(Order::Bundle(bundle_b));
		op.insert(Order::Bundle(bundle_a));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}

	#[test]
	fn test_pool_iterator_multiple_reverse_order_b() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle_b));
		op.insert(Order::Bundle(bundle_a));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}
}
