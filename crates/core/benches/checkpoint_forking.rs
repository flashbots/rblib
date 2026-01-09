//! Benchmark: Checkpoint Forking/Branching
//!
//! This benchmark measures the cost of creating and using parallel branches
//! from a shared checkpoint.
//!
//! ## Scenario
//!
//! ```text
//! Base chain:     [0] -> [1] -> ... -> [M] (fork point)
//!                                        |
//! Branch A:                              +-> [A1] -> ... -> [A10]
//! Branch B:                              +-> [B1] -> ... -> [B10]
//! Branch C:                              +-> [C1] -> ... -> [C10]
//! ```
//!
//! Each branch applies 10 transactions from a unique signer (no state
//! contention).

use {
	criterion::{BenchmarkId, Criterion, criterion_group, criterion_main},
	rblib_core::{
		alloy::primitives::{Address, U256},
		payload::{BlockContext, Checkpoint},
		platform::Ethereum,
		reth::primitives::Recovered,
		test_utils::{BlockContextMocked, FundedAccounts, transfer_tx_to},
	},
};

type Transaction = rblib_core::platform::types::Transaction<Ethereum>;

/// Base chain depths to test forking from.
const FORK_DEPTHS: [u64; 3] = [10, 100, 500];

/// Number of parallel branches to create from fork point.
/// To test more than 10, we should add new prefunded signers (to avoid state
/// contention).
const BRANCH_COUNTS: [usize; 3] = [2, 5, 10];

/// Transactions to apply per branch.
const TXS_PER_BRANCH: u64 = 10;

/// Build a checkpoint chain of N transactions using signer 0.
fn build_base_chain(n: u64) -> Checkpoint<Ethereum> {
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

/// Generate transactions for a branch using a specific signer.
/// Each branch uses a different signer to avoid state contention.
fn generate_branch_txs(signer_index: u32) -> Vec<Recovered<Transaction>> {
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

/// Benchmark forking and applying transactions to multiple branches.
///
/// Measures the total cost of:
/// 1. Cloning the fork point N times (Arc clone)
/// 2. Applying 10 transactions to each branch
fn bench_fork_and_apply(c: &mut Criterion) {
	let mut group = c.benchmark_group("fork_and_apply");

	for depth in FORK_DEPTHS {
		for num_branches in BRANCH_COUNTS {
			let branch_txs: Vec<Vec<Recovered<Transaction>>> = (1..=num_branches)
				.map(|signer_idx| generate_branch_txs(signer_idx as u32))
				.collect();

			let fork_point = build_base_chain(depth);

			let id = BenchmarkId::new(
				format!("base_{depth}_txs"),
				format!("{num_branches}_branches_10tx_each"),
			);

			group.bench_with_input(
				id,
				&(&fork_point, &branch_txs),
				|b, (fork_point, branch_txs)| {
					b.iter(|| {
						let mut branches = Vec::with_capacity(branch_txs.len());

						for txs in branch_txs.iter() {
							// Fork from the shared checkpoint
							let mut branch = (*fork_point).clone();

							// Apply transactions to this branch
							for tx in txs {
								branch = branch.apply(tx.clone()).unwrap();
							}

							branches.push(branch);
						}

						branches
					});
				},
			);
		}
	}

	group.finish();
}

/// Benchmark just the fork (clone) operation without applying transactions.
///
/// Isolates the cost of checkpoint cloning to verify Arc efficiency.
fn bench_fork_only(c: &mut Criterion) {
	let mut group = c.benchmark_group("clone_10_forks");

	for depth in FORK_DEPTHS {
		let fork_point = build_base_chain(depth);
		let param = format!("base_{depth}_txs");

		group.bench_with_input(
			BenchmarkId::from_parameter(&param),
			&fork_point,
			|b, fork_point| {
				b.iter(|| {
					// Clone the checkpoint 10 times
					let forks: Vec<_> = (0..10).map(|_| fork_point.clone()).collect();
					forks
				});
			},
		);
	}

	group.finish();
}

criterion_group!(benches, bench_fork_and_apply, bench_fork_only);
criterion_main!(benches);
