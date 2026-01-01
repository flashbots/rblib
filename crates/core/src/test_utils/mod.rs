//! rblib-core testing infrastructure
//!
//! Those are utilities that provide ways to test platforms and other primitives
//! defined by rblib-core and downstream crates extending and building on top of
//! rblib such as rblib-pipeline.
//!
//! By default, two test platforms are defined for the two platforms maintained
//! by flashbots:
//! - [`Ethereum`]
//! - [`Optimism`]
//!
//! If you want to make your own [`crate::Platform`] type implementation
//! available for testing with those utils, you need to implement the
//! [`TestablePlatform`] trait for it.

mod accounts;

mod exts;

mod mock;

mod transactions;

pub use {
	accounts::{FundedAccounts, WithFundedAccounts},
	exts::*,
	mock::{GenesisProviderFactory, GenesisStateProvider},
	transactions::{
		invalid_tx,
		reverting_tx,
		test_bundle,
		test_tx,
		test_txs,
		transfer_tx,
	},
};

pub const ONE_ETH: u128 = 1_000_000_000_000_000_000;
pub const TEST_COINBASE: crate::alloy::primitives::Address = //
	crate::alloy::primitives::address!(
		"0x0000000000000000000000000000000000012345"
	);

#[cfg(feature = "optimism")]
pub mod optimism_constants {
	pub const DEFAULT_DENOMINATOR: u32 = 50;
	pub const DEFAULT_ELASTICITY: u32 = 2;
	pub const DEFAULT_EIP_1559_PARAMS: u64 =
		((DEFAULT_DENOMINATOR as u64) << 32) | (DEFAULT_ELASTICITY as u64);
	pub const DEFAULT_MIN_BASE_FEE: u64 = 0;
}
