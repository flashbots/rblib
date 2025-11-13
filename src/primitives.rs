use crate::alloy::primitives::Address;

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
