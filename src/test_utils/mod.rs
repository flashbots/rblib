//! rblib testing infrastructure
//!
//! Those are utilities that provide ways to test steps, pipelines, platforms
//! and other primitives defined by rblib and downstream crates extending and
//! building on top of rblib.
//!
//! By default, two test platforms are defined for the two platforms that ship
//! with rblib out of the box:
//! - [`Ethereum`]
//! - [`Optimism`]
//!
//! If you want to make your own [`crate::Platform`] type implementation
//! available for testing with those utils you need to implement the
//! [`TestablePlatform`] trait for it.

mod accounts;
mod ethereum;
mod exts;
mod mock;
mod node;
mod platform;
mod step;
mod transactions;

#[allow(unused_imports)]
pub(crate) use step::fake_step;
pub use {
	accounts::{FundedAccounts, WithFundedAccounts},
	ethereum::EthConsensusDriver,
	exts::*,
	mock::{GenesisProviderFactory, GenesisStateProvider},
	node::{ConsensusDriver, LocalNode},
	platform::{TestNodeFactory, TestablePlatform},
	rblib_tests_macros::{assert_is_dyn_safe, if_platform, rblib_test},
	step::{AlwaysBreakStep, AlwaysFailStep, AlwaysOkStep, OneStep},
	transactions::{
		invalid_tx,
		reverting_tx,
		test_bundle,
		test_tx,
		test_txs,
		transfer_tx,
	},
};

#[cfg(feature = "optimism")]
mod optimism;

#[cfg(feature = "optimism")]
pub use optimism::OptimismConsensusDriver;

pub const ONE_ETH: u128 = 1_000_000_000_000_000_000;
pub const TEST_COINBASE: crate::alloy::primitives::Address = //
	crate::alloy::primitives::address!(
		"0x0000000000000000000000000000000000012345"
	);

/// This gets invoked before any tests, when the cargo test framework loads the
/// test library.
#[cfg(feature = "test-utils")]
#[ctor::ctor]
fn init_test_logging() {
	use tracing_subscriber::{filter::filter_fn, prelude::*};
	if let Ok(v) = std::env::var("TEST_TRACE") {
		let level = match v.as_str() {
			"true" | "debug" | "on" => tracing::Level::DEBUG,
			"trace" => tracing::Level::TRACE,
			"info" => tracing::Level::INFO,
			"warn" => tracing::Level::WARN,
			"error" => tracing::Level::ERROR,
			_ => return,
		};

		let prefix_blacklist = &[
			"storage::db::mdbx",
			"alloy_transport_ipc",
			"trie",
			"engine",
			"reth::cli",
			"reth_tasks",
			"static_file",
			"txpool",
			"provider::static_file",
			"reth_node_builder::launch::common",
			"reth_db_common",
			"pruner",
			"reth_node_events",
			"provider::storage_writer",
			"providers::db",
		];

		tracing_subscriber::registry()
			.with(tracing_subscriber::fmt::layer())
			.with(filter_fn(move |metadata| {
				metadata.level() <= &level
					&& !prefix_blacklist
						.iter()
						.any(|prefix| metadata.target().starts_with(prefix))
			}))
			.init();
	}
}
