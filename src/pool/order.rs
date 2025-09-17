use {
	super::*,
	crate::{alloy, reth},
	alloy::{
		consensus::{Transaction, crypto::RecoveryError},
		primitives::{Address, B256},
	},
	derive_more::Deref,
	nonce::Nonce,
	reth::{ethereum::primitives::SignedTransaction, primitives::Recovered},
	std::collections::{HashMap, HashSet},
};

pub type OrderHash = B256;

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

	pub fn nonces(&self) -> impl Iterator<Item = (Address, Nonce)> {
		self
			.transactions()
			.iter()
			.map(|tx| (tx.signer(), tx.nonce()))
	}
}

#[derive(Debug, Clone, Deref)]
pub struct PooledOrder<P: Platform>(Arc<PooledOrderInner<P>>);

impl<P: Platform> PooledOrder<P> {
	pub fn hash(&self) -> OrderHash {
		self.0.hash
	}
}

impl<P: Platform> PartialEq for PooledOrder<P> {
	fn eq(&self, other: &Self) -> bool {
		self.hash == other.hash
	}
}
impl<P: Platform> Eq for PooledOrder<P> {}

impl<P: Platform> std::hash::Hash for PooledOrder<P> {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.hash.hash(state);
	}
}

impl<P: Platform> From<Order<P>> for PooledOrder<P> {
	fn from(order: Order<P>) -> Self {
		Self::new(order)
	}
}

impl<P: Platform> From<PooledOrder<P>> for Order<P> {
	fn from(pooled: PooledOrder<P>) -> Self {
		pooled.order.clone()
	}
}

impl<P: Platform> PooledOrder<P> {
	pub fn new(order: Order<P>) -> Self {
		Self(Arc::new(PooledOrderInner {
			hash: order.hash(),
			order,
			ancestors: HashMap::new(),
			descendants: HashMap::new(),
		}))
	}
}

impl<P: Platform> PooledOrder<P> {
	pub fn discard(&self) {}

	pub fn commit(&self) {}
}

#[derive(Debug, Deref)]
pub struct PooledOrderInner<P: Platform> {
	#[deref]
	pub order: Order<P>,
	pub hash: OrderHash,
	pub ancestors: HashMap<OrderHash, PooledOrder<P>>,
	pub descendants: HashMap<OrderHash, PooledOrder<P>>,
}
