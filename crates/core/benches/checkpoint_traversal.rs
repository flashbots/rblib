//! Benchmark: Checkpoint Chain Depth Traversal
//!
//! This benchmark measures how state lookup cost scales with checkpoint chain
//! depth, isolating the traversal overhead of the checkpoint abstraction.
//!
//! ## Read Patterns
//!
//! - **Hot**: Account modified in most recent checkpoint (minimal traversal)
//! - **Cold**: Account only in base state (full chain traversal)
//! - **Deep**: Account modified early in chain (partial traversal)

use {
	criterion::{BenchmarkId, Criterion, criterion_group, criterion_main},
	rblib_core::{
		alloy::primitives::Address,
		payload::{BlockContext, Checkpoint},
		platform::Ethereum,
		reth::{primitives::Recovered, revm::DatabaseRef},
		test_utils::{BlockContextMocked, FundedAccounts, transfer_tx_to},
	},
};

type Transaction = rblib_core::platform::types::Transaction<Ethereum>;

const DEPTHS: [u64; 5] = [10, 50, 100, 500, 1000];

/// Build a checkpoint chain of N transactions, returning the final checkpoint
/// and the recipient address from the first transaction (for deep reads).
fn build_checkpoint_chain(n: u64) -> (Checkpoint<Ethereum>, Address) {
	let block = BlockContext::<Ethereum>::mocked();
	let mut checkpoint = block.start();

	let signer = FundedAccounts::signer(0);
	let mut first_recipient = Address::ZERO;

	for nonce in 0..n {
		let to = Address::random();
		let tx: Recovered<Transaction> = transfer_tx_to::<Ethereum>(
			&signer,
			nonce,
			rblib_core::alloy::primitives::U256::from(1u64),
			to,
		);

		// Capture the first recipient for deep read tests
		// TODO: we can improve this because deep is really close to base here
		if nonce == 0 {
			first_recipient = to
		}

		checkpoint = checkpoint.apply(tx).unwrap();
	}

	(checkpoint, first_recipient)
}

/// Benchmark "hot" reads - account touched in every checkpoint.
///
/// The signer account is modified by every transaction in the chain,
/// so it should be found quickly near the top of the checkpoint chain.
fn bench_hot_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("read_hot_account");
	let signer_address = FundedAccounts::address(0);

	for depth in DEPTHS {
		let (checkpoint, _) = build_checkpoint_chain(depth);
		let param = format!("chain_depth_{depth}");

		group.bench_with_input(
			BenchmarkId::from_parameter(&param),
			&checkpoint,
			|b, checkpoint| {
				b.iter(|| checkpoint.basic_ref(signer_address));
			},
		);
	}

	group.finish();
}

/// Benchmark "cold" reads - account only in base state.
///
/// Reading an untouched account requires traversing the entire checkpoint
/// chain before falling through to the base state.
fn bench_cold_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("read_untouched_account");

	// Use an account never touched in any transaction
	let untouched_address = Address::repeat_byte(0xDE);

	for depth in DEPTHS {
		let (checkpoint, _) = build_checkpoint_chain(depth);
		let param = format!("chain_depth_{depth}");

		group.bench_with_input(
			BenchmarkId::from_parameter(&param),
			&checkpoint,
			|b, checkpoint| {
				b.iter(|| checkpoint.basic_ref(untouched_address));
			},
		);
	}

	group.finish();
}

/// Benchmark "deep" reads - account modified early in the chain.
///
/// The recipient of the first transaction is only present in the bottom-most
/// checkpoint, requiring traversal through (depth - 1) checkpoints.
fn bench_deep_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("read_deep_account");

	for depth in DEPTHS {
		let (checkpoint, first_recipient) = build_checkpoint_chain(depth);
		let param = format!("chain_depth_{depth}");

		group.bench_with_input(
			BenchmarkId::from_parameter(&param),
			&(checkpoint, first_recipient),
			|b, (checkpoint, addr)| {
				b.iter(|| checkpoint.basic_ref(*addr));
			},
		);
	}

	group.finish();
}

criterion_group!(benches, bench_hot_read, bench_cold_read, bench_deep_read);
criterion_main!(benches);
