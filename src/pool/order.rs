use {
	super::*,
	crate::{alloy, reth},
	alloy::{
		consensus::{Transaction, crypto::RecoveryError},
		primitives::{Address, B256},
	},
	core::sync::atomic::AtomicUsize,
	nonce::Nonce,
	reth::{ethereum::primitives::SignedTransaction, primitives::Recovered},
	std::collections::{HashMap, HashSet},
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
	pub fn signers(&self) -> HashSet<Address> {
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
		Self(Arc::new(PooledOrderInner {
			hash: order.hash(),
			order,
			group: Arc::new(Group::default()),
			ancestors: HashMap::new(),
			descendants: HashMap::new(),
			conflicts: HashMap::new(),
		}))
	}
}

impl<P: Platform> PooledOrder<P> {
	pub fn discard(&self) {}

	pub fn commit(&self) {}
}

#[derive(Debug)]
struct PooledOrderInner<P: Platform> {
	/// The underlying order.
	pub order: Order<P>,

	/// The hash of the order.
	pub hash: OrderHash,

	/// The group this order belongs to. All orders in the same group share at
	/// least one signer address.
	pub group: Arc<Group<P>>,

	/// Orders that must be included before this order can be included.
	/// All ancestors will belong to the same group as this order.
	pub ancestors: HashMap<OrderHash, PooledOrder<P>>,

	/// Orders that depend on this order being included first.
	/// All descendants will belong to the same group as this order.
	pub descendants: HashMap<OrderHash, PooledOrder<P>>,

	/// Orders that conflict with this order. For example, two orders that
	/// attempt to spend the same nonce for the same signer will be in conflict.
	///
	/// If this order is committed, all conflicting orders must be discarded.
	///
	/// All conflicting orders will belong to the same group as this order.
	pub conflicts: HashMap<OrderHash, PooledOrder<P>>,
}

/// Represents a group of orders that have nonce relationships between them.
/// See the [`NonceRelation`] documentation for more details on the types of
/// nonce relationships that can exist between orders.
#[derive(Debug, Default)]
pub struct Group<P: Platform> {
	/// The list of addresses that are signers on orders in this group.
	/// Each signed transaction in an order increments the reference count for
	/// its signer address. When an order is removed from the pool, the
	/// reference counts for its signers are decremented.
	///
	/// This is used to quickly identify if an order belongs to this group.
	signers: HashMap<Address, AtomicUsize>,

	/// All orders that are part of this group.
	orders: HashMap<OrderHash, PooledOrder<P>>,
}

impl<P: Platform> Group<P> {
	/// Tests whether the given order belongs to this group.
	/// An order belongs to a group if it shares at least one signer with any
	/// order already in the group.
	pub fn belongs(&self, order: &Order<P>) -> bool {
		order
			.signers()
			.iter()
			.any(|signer| self.signers.contains_key(signer))
	}

	/// Returns an iterator over all orders in this group that have no ancestors.
	pub fn roots(&self) -> impl Iterator<Item = &PooledOrder<P>> {
		self
			.orders
			.values()
			.filter(|order| order.0.ancestors.is_empty())
	}
}
