//! Shared scenario definitions for `revm_regression` benchmarks.
//!
//! This module defines the test scenarios used to measure the cost of
//! abstraction of the Checkpoint system against direct REVM/BundleState usage.
//!
//! # Scenarios
//!
//! ## Apply
//!
//! Measures transaction execution overhead.
//!
//! **Workload**: N simple ETH transfers (21k gas each) from a single funded
//! account to random recipients.
//!
//! **Approaches compared**:
//! - `checkpoint_chain`: Each tx creates a new immutable Checkpoint via
//!   `checkpoint.apply(tx)`. State lookups traverse the checkpoint chain.
//! - `single_state`: All txs applied to one mutable `State` via
//!   `transact_commit()`. Single `BundleState` accumulates all changes.
//!
//! **What it measures**: Per-transaction overhead of checkpoint creation and
//! the growing cost of state lookups as the chain deepens.
//!
//! ## Revert
//!
//! Measures the cost of maintaining revert capability.
//!
//! **Workload**: Apply-revert-continue pattern. For each pair: apply a "kept"
//! tx, apply a "reverted" tx, then discard the second.
//!
//! **Approaches compared**:
//! - `checkpoint`: Revert by keeping reference to previous checkpoint (O(1)).
//! - `direct_per_tx_merge`: Merge after each tx, use
//!   `BundleState::revert_latest()`.
//! - `direct_batch_baseline`: No revert capability, only applies kept txs
//!   (baseline).
//!
//! **What it measures**: Cost of revert capability. Checkpoint revert is O(1)
//! but pays traversal cost on subsequent reads. Direct revert is O(accounts)
//! but has O(1) reads.
//!
//! ## Traversal
//!
//! Measures state lookup cost at varying checkpoint chain depths.
//!
//! **Workload**: Build a checkpoint chain of depth N, then perform single
//! account reads.
//!
//! **Read patterns**:
//! - `hot`: Read the signer account (modified in every checkpoint, found at
//!   top).
//! - `cold`: Read an untouched account (must traverse entire chain to base
//!   state).
//! - `deep`: Read recipient of first tx (found at bottom of chain).
//!
//! **What it measures**: How read latency scales with chain depth. Cold reads
//! show worst-case traversal cost. Hot reads show best-case.
//!
//! ## Forking
//!
//! Measures the cost of creating parallel branches from a shared checkpoint.
//!
//! **Workload**: Build a base chain of depth M, then create N branches from the
//! tip, each applying 10 transactions from a unique signer (no state
//! contention).
//!
//! ```text
//! Base: [0] -> [1] -> ... -> [M]
//!                             |-> Branch A: 10 txs
//!                             |-> Branch B: 10 txs
//!                             |-> Branch C: 10 txs
//! ```
//!
//! **What it measures**: Cost of checkpoint cloning (Arc efficiency) and
//! whether parallel branches interfere with each other's performance.

#![allow(dead_code, unreachable_pub)]

use rblib_core::{
	alloy::primitives::{Address, U256},
	payload::{BlockContext, Checkpoint},
	platform::Ethereum,
	reth::primitives::Recovered,
	test_utils::{
		BlockContextMocked,
		FundedAccounts,
		transfer_tx,
		transfer_tx_to,
	},
};

pub type Transaction = rblib_core::platform::types::Transaction<Ethereum>;

// ============================================================================
// Apply scenario
// ============================================================================

pub const APPLY_SAMPLES: [u64; 4] = [10, 100, 500, 1000];

pub struct ApplySetup {
	pub block: BlockContext<Ethereum>,
	pub txs: Vec<Recovered<Transaction>>,
}

pub fn generate_transactions(n: u64) -> Vec<Recovered<Transaction>> {
	let signer = FundedAccounts::signer(0);
	(0..n)
		.map(|nonce| transfer_tx::<Ethereum>(&signer, nonce, U256::from(1u64)))
		.collect()
}

pub fn apply_setup(n: u64) -> ApplySetup {
	ApplySetup {
		block: BlockContext::<Ethereum>::mocked(),
		txs: generate_transactions(n),
	}
}

pub fn apply_setup_10() -> ApplySetup {
	apply_setup(10)
}
pub fn apply_setup_100() -> ApplySetup {
	apply_setup(100)
}
pub fn apply_setup_500() -> ApplySetup {
	apply_setup(500)
}
pub fn apply_setup_1000() -> ApplySetup {
	apply_setup(1000)
}

// ============================================================================
// Revert scenario
// ============================================================================

pub const REVERT_PAIR_COUNTS: [u64; 4] = [5, 25, 100, 250];

pub struct RevertSetup {
	pub block: BlockContext<Ethereum>,
	pub pairs: Vec<(Recovered<Transaction>, Recovered<Transaction>)>,
}

pub fn generate_transaction_pairs(
	num_pairs: u64,
) -> Vec<(Recovered<Transaction>, Recovered<Transaction>)> {
	let signer = FundedAccounts::signer(0);
	(0..num_pairs)
		.map(|i| {
			(
				transfer_tx::<Ethereum>(&signer, i, U256::from(1u64)),
				transfer_tx::<Ethereum>(&signer, i + 1, U256::from(1u64)),
			)
		})
		.collect()
}

pub fn revert_setup(num_pairs: u64) -> RevertSetup {
	RevertSetup {
		block: BlockContext::<Ethereum>::mocked(),
		pairs: generate_transaction_pairs(num_pairs),
	}
}

pub fn revert_setup_5() -> RevertSetup {
	revert_setup(5)
}
pub fn revert_setup_25() -> RevertSetup {
	revert_setup(25)
}
pub fn revert_setup_100() -> RevertSetup {
	revert_setup(100)
}
pub fn revert_setup_250() -> RevertSetup {
	revert_setup(250)
}

// ============================================================================
// Traversal scenario
// ============================================================================

pub const TRAVERSAL_DEPTHS: [u64; 5] = [10, 50, 100, 500, 1000];

pub fn build_checkpoint_chain(n: u64) -> (Checkpoint<Ethereum>, Address) {
	let block = BlockContext::<Ethereum>::mocked();
	let mut checkpoint = block.start();
	let signer = FundedAccounts::signer(0);
	let mut first_recipient = Address::ZERO;

	for nonce in 0..n {
		let to = Address::random();
		let tx: Recovered<Transaction> =
			transfer_tx_to::<Ethereum>(&signer, nonce, U256::from(1u64), to);

		if nonce == 0 {
			first_recipient = to;
		}
		checkpoint = checkpoint.apply(tx).unwrap();
	}

	(checkpoint, first_recipient)
}

// ============================================================================
// Forking scenario
// ============================================================================

pub const FORK_DEPTHS: [u64; 3] = [10, 100, 500];
pub const BRANCH_COUNTS: [usize; 3] = [2, 5, 10];
pub const TXS_PER_BRANCH: u64 = 10;

pub fn build_base_chain(n: u64) -> Checkpoint<Ethereum> {
	let block = BlockContext::<Ethereum>::mocked();
	let mut checkpoint = block.start();
	let signer = FundedAccounts::signer(0);

	for nonce in 0..n {
		let tx: Recovered<Transaction> = transfer_tx_to::<Ethereum>(
			&signer,
			nonce,
			U256::from(1u64),
			Address::random(),
		);
		checkpoint = checkpoint.apply(tx).unwrap();
	}

	checkpoint
}

pub fn generate_branch_txs(signer_index: u32) -> Vec<Recovered<Transaction>> {
	let signer = FundedAccounts::signer(signer_index);
	(0..TXS_PER_BRANCH)
		.map(|nonce| {
			transfer_tx_to::<Ethereum>(
				&signer,
				nonce,
				U256::from(1u64),
				Address::random(),
			)
		})
		.collect()
}
