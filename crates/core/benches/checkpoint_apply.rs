//! Benchmark: Checkpoint Apply vs. direct REVM Execution with a single
//! BundleState
//!
//! This benchmark compares the performance overhead of the checkpoint
//! abstraction against direct EVM execution.
//!
//! ## State Characteristics
//!
//! - **Sender**: Single funded account (FundedAccounts::signer(0))
//! - **Recipients**: Random address per transaction (new account each time)
//! - **Transaction**: Simple ETH transfer (21,000 gas)
//! - **Contention**: Low, sender touched N times, N new recipients created
//!
//! ## Approaches Compared
//!
//! 1. **Checkpoint**: `checkpoint.apply(tx)` loop
//!    - Creates a new Checkpoint per transaction
//!    - State lookups traverse the checkpoint chain
//!    - Immutable history preserved
//!
//! 2. **Direct**: `transact_commit(&tx)` loop on a single State
//!    - Single BundleState instance, mutations accumulate
//!    - No checkpoint chain overhead
//!    - All changes in one BundleState

use {
	criterion::{
		BenchmarkId,
		Criterion,
		Throughput,
		criterion_group,
		criterion_main,
	},
	rblib_core::{
		payload::BlockContext,
		platform::Ethereum,
		reth::{
			evm::{ConfigureEvm, Evm},
			primitives::Recovered,
			revm::{
				State,
				db::{WrapDatabaseRef, states::bundle_state::BundleRetention},
			},
		},
		test_utils::{BlockContextMocked, FundedAccounts, transfer_tx},
	},
};

type Transaction = rblib_core::platform::types::Transaction<Ethereum>;
const SAMPLES: [u64; 4] = [10, 100, 500, 1000];

/// Generate N transfer transactions from account 0 with sequential nonces.
fn generate_transactions(n: u64) -> Vec<Recovered<Transaction>> {
	let signer = FundedAccounts::signer(0);
	(0..n)
		.map(|nonce| {
			transfer_tx::<Ethereum>(
				&signer,
				nonce,
				rblib_core::alloy::primitives::U256::from(1u64),
			)
		})
		.collect()
}

/// Benchmark comparing checkpoint.apply() vs direct transact_commit.
/// Same group for comparison
fn bench_checkpoint_vs_direct(c: &mut Criterion) {
	let mut group = c.benchmark_group("tx_execution");

	for n in SAMPLES {
		let txs = generate_transactions(n);
		let param = format!("{n}_transfers");

		group.throughput(Throughput::Elements(n));

		// Checkpoint approach: each tx creates a new immutable checkpoint
		group.bench_with_input(
			BenchmarkId::new("checkpoint_chain", &param),
			&txs,
			|b, txs| {
				b.iter_with_setup(
					|| {
						let block = BlockContext::<Ethereum>::mocked();
						let checkpoint = block.start();
						(block, checkpoint, txs.clone())
					},
					|(_block, mut checkpoint, txs)| {
						for tx in txs {
							checkpoint = checkpoint.apply(tx).unwrap();
						}
						checkpoint
					},
				);
			},
		);

		// Direct approach: all txs on a single mutable State
		group.bench_with_input(
			BenchmarkId::new("single_state", &param),
			&txs,
			|b, txs| {
				b.iter_with_setup(
					|| {
						let block = BlockContext::<Ethereum>::mocked();
						(block, txs.clone())
					},
					|(block, txs)| {
						let base_state = block.start();

						let mut state = State::builder()
							.with_database(WrapDatabaseRef(base_state))
							.with_bundle_update()
							.build();

						let evm_config = block.evm_config();
						let evm_env = block.evm_env();

						for tx in txs {
							evm_config
								.evm_with_env(&mut state, evm_env.clone())
								.transact_commit(&tx)
								.expect("transaction execution failed");
						}

						state.merge_transitions(BundleRetention::Reverts);
						state.take_bundle()
					},
				);
			},
		);
	}

	group.finish();
}

criterion_group!(benches, bench_checkpoint_vs_direct);
criterion_main!(benches);
