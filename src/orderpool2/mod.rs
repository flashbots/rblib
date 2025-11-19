use {crate::alloy::primitives::Address, std::hash::Hash};

pub mod prioritized_pool;
pub mod sim_tree;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccountNonce {
	pub account: Address,
	pub nonce: u64,
}

impl AccountNonce {
	#[must_use]
	pub fn with_nonce(self, nonce: u64) -> Self {
		Self {
			account: self.account,
			nonce,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleNonce {
	pub address: Address,
	pub nonce: u64,
	pub optional: bool,
}

pub trait OrderpoolOrder {
	type ID: Hash + Eq;
	fn id(&self) -> Self::ID;
	fn nonces(&self) -> Vec<BundleNonce>;
}

pub trait OrderpoolNonceSource {
	type NonceError;
	fn nonce(&self, address: &Address) -> Result<u64, Self::NonceError>;
}
