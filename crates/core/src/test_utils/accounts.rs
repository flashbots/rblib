use {
	crate::{
		alloy::{
			genesis::{Genesis, GenesisAccount},
			primitives::{Address, U256},
			signers::local::PrivateKeySigner,
		},
		reth::{
			chainspec::ChainSpec,
			ethereum::EthPrimitives,
			providers::test_utils::{ExtendedAccount, MockEthProvider},
		},
		test_utils::ONE_ETH,
	},
	std::sync::Arc,
};

/// Those accounts are defined in the genesis block of the test local node,
/// each prefunded with 100 ETH and nonces starting from 0.
pub struct FundedAccounts;

#[allow(clippy::cast_possible_truncation)]
impl FundedAccounts {
	const FUNDED_PRIVATE_KEYS: &[&str] = &[
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff81",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff82",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff83",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff84",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff85",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff86",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff87",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff88",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff89",
		"0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff90",
	];

	/// Returns a signer for the given key index.
	///
	/// # Panics
	/// If the key index is out of bounds and greater than the number of
	/// predefined accounts.
	pub fn signer(key: u32) -> PrivateKeySigner {
		assert!(
			(key as usize) < Self::FUNDED_PRIVATE_KEYS.len(),
			"Key index out of bounds, must be less than {}",
			Self::FUNDED_PRIVATE_KEYS.len()
		);

		Self::FUNDED_PRIVATE_KEYS[key as usize]
			.parse()
			.expect("invalid hardcoded builder private key")
	}

	/// Returns the signer index for the given address or None if the address is
	/// not one of the predefined accounts.
	///
	/// # Panics
	/// Should never panic as the keys are hardcoded and should always be valid
	/// and parsing them should not fail.
	pub fn index(address: Address) -> Option<u32> {
		Self::FUNDED_PRIVATE_KEYS
			.iter()
			.position(|&secret| {
				secret
					.parse::<PrivateKeySigner>()
					.expect("invalid hardcoded builder private key")
					.address()
					== address
			})
			.map(|i| i as u32)
	}

	pub fn address(key: u32) -> Address {
		Self::signer(key).address()
	}

	pub fn random() -> PrivateKeySigner {
		let key = rand::random::<u32>() % Self::FUNDED_PRIVATE_KEYS.len() as u32;
		Self::signer(key)
	}

	pub fn addresses() -> impl Iterator<Item = Address> {
		Self::FUNDED_PRIVATE_KEYS
			.iter()
			.enumerate()
			.map(|(i, _)| Self::address(i as u32))
	}

	pub fn signers() -> impl Iterator<Item = PrivateKeySigner> {
		Self::FUNDED_PRIVATE_KEYS
			.iter()
			.enumerate()
			.map(|(i, _)| Self::signer(i as u32))
	}

	pub fn by_address(address: Address) -> Option<PrivateKeySigner> {
		Self::signers().find(|signer| signer.address() == address)
	}

	pub const fn len() -> usize {
		Self::FUNDED_PRIVATE_KEYS.len()
	}
}

/// A Helper trait used to extend various test and mock types with
/// the predefined funded accounts each funded with 100 ETH.
pub trait WithFundedAccounts {
	#[must_use]
	fn with_funded_accounts(self) -> Self;
}

/// Extension trait for `MockEthProvider` to add funded accounts.
impl<ChainSpec> WithFundedAccounts
	for MockEthProvider<EthPrimitives, ChainSpec>
{
	fn with_funded_accounts(self) -> Self {
		self.extend_accounts(FundedAccounts::signers().map(|s| {
			(
				s.address(),
				ExtendedAccount::new(0, U256::from(100 * ONE_ETH)),
			)
		}));
		self
	}
}

impl WithFundedAccounts for ChainSpec {
	fn with_funded_accounts(self) -> Self {
		let mut chainspec = self;
		chainspec.genesis = chainspec.genesis.with_funded_accounts();
		chainspec
	}
}

impl WithFundedAccounts for Arc<ChainSpec> {
	fn with_funded_accounts(self) -> Self {
		let mut chainspec = Arc::unwrap_or_clone(self);
		chainspec.genesis = chainspec.genesis.with_funded_accounts();
		Arc::new(chainspec)
	}
}

#[cfg(feature = "optimism")]
impl WithFundedAccounts for crate::reth::optimism::chainspec::OpChainSpec {
	fn with_funded_accounts(self) -> Self {
		let mut chainspec = self;
		chainspec.inner.genesis = chainspec.inner.genesis.with_funded_accounts();
		chainspec
	}
}

#[cfg(feature = "optimism")]
impl WithFundedAccounts for Arc<crate::reth::optimism::chainspec::OpChainSpec> {
	fn with_funded_accounts(self) -> Self {
		let chainspec = Arc::unwrap_or_clone(self).with_funded_accounts();
		Arc::new(chainspec)
	}
}

impl WithFundedAccounts for Genesis {
	fn with_funded_accounts(self) -> Self {
		self.extend_accounts(FundedAccounts::addresses().map(|address| {
			(
				address,
				GenesisAccount::default().with_balance(U256::from(100 * ONE_ETH)),
			)
		}))
	}
}
