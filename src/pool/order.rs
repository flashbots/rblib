use {
	super::*,
	crate::{alloy, reth},
	alloy::{
		consensus::{Transaction, crypto::RecoveryError},
		primitives::{Address, B256},
	},
	core::ops::Deref,
	nonce::Nonce,
	reth::{ethereum::primitives::SignedTransaction, primitives::Recovered},
	rustc_hash::FxHashSet,
};

pub type OrderHash = B256;

/// Represents an order submitted to the order pool. An order can be either a
/// single transaction or a bundle of transactions. This is the publicly facing
/// type that users of the pool will interact with.
///
/// Internally, orders are wrapped in `PooledOrder` which adds metadata
/// required for managing the order within the pool, such as tracking ancestor
/// and descendant relationships with other orders.
#[derive(Debug, Clone)]
pub enum Order<P: Platform> {
	/// A single transaction.
	Transaction(Recovered<types::Transaction<P>>),

	/// A bundle of transactions.
	Bundle(types::Bundle<P>),
}

impl<P: Platform> Order<P> {
	pub fn hash(&self) -> OrderHash {
		match self {
			Order::Transaction(tx) => *tx.tx_hash(),
			Order::Bundle(bundle) => bundle.hash(),
		}
	}

	/// Returns a slice of all transactions contained in this order.
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

	/// Returns an iterator over all unique signers in this order.
	pub fn signers(&self) -> FxHashSet<Address> {
		self.transactions().iter().map(|tx| tx.signer()).collect()
	}

	/// Returns an iterator over all (signer, nonce) pairs in this order.
	/// If the signer appears multiple times, it will appear multiple times in the
	/// output.
	pub fn nonces(&self) -> impl Iterator<Item = (Address, Nonce)> {
		self
			.transactions()
			.iter()
			.map(|tx| (tx.signer(), tx.nonce()))
	}
}

/// An order that has entered the pool. This wraps an `Order` and adds metadata
/// required for managing the order within the pool.
///
/// Notes:
/// - In `OrderPool`, orders are self-managed entities that track their own
///   relationships with other orders, their scores and their center of gravity
///   within the pool.
///
/// - `PooledOrder` instances are cheap to clone. All clones share the same
///   underlying data and will modify the same internal state.
#[derive(Debug, Clone)]
pub struct PooledOrder<P: Platform>(Arc<PooledOrderInner<P>>);

impl<P: Platform> PooledOrder<P> {
	pub fn hash(&self) -> OrderHash {
		self.0.hash
	}

	pub fn signers(&self) -> &FxHashSet<Address> {
		&self.0.signers
	}
}

impl<P: Platform> Deref for PooledOrder<P> {
	type Target = Order<P>;

	fn deref(&self) -> &Self::Target {
		&self.0.order
	}
}

impl<P: Platform> PartialEq for PooledOrder<P> {
	fn eq(&self, other: &Self) -> bool {
		self.0.hash == other.0.hash
	}
}
impl<P: Platform> Eq for PooledOrder<P> {}

impl<P: Platform> std::hash::Hash for PooledOrder<P> {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.0.hash.hash(state);
	}
}

impl<P: Platform> From<Order<P>> for PooledOrder<P> {
	fn from(order: Order<P>) -> Self {
		Self::new(order)
	}
}

impl<P: Platform> From<PooledOrder<P>> for Order<P> {
	fn from(pooled: PooledOrder<P>) -> Self {
		pooled.0.order.clone()
	}
}

impl<P: Platform> PooledOrder<P> {
	pub fn new(order: Order<P>) -> Self {
		debug_assert!(!order.transactions().is_empty());
		Self(Arc::new(PooledOrderInner {
			hash: order.hash(),
			signers: order.signers(),
			order,
		}))
	}
}

#[derive(Debug)]
struct PooledOrderInner<P: Platform> {
	/// The underlying order.
	pub order: Order<P>,

	/// The hash of the order.
	pub hash: OrderHash,

	/// The unique set of signers in this order.
	pub signers: FxHashSet<Address>,
}
