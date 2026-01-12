//! Criterion benchmark: state lookup cost at varying checkpoint chain depths.
//!
//! Measures how read performance scales with chain depth for hot, cold, and
//! deep accounts.

mod scenarios;

use {
	criterion::{BenchmarkId, Criterion, criterion_group, criterion_main},
	rblib_core::{
		alloy::primitives::Address,
		reth::revm::DatabaseRef,
		test_utils::FundedAccounts,
	},
	scenarios::{TRAVERSAL_DEPTHS, build_checkpoint_chain},
};

fn bench_hot_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("read_hot_account");
	let signer_address = FundedAccounts::address(0);

	for depth in TRAVERSAL_DEPTHS {
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

fn bench_cold_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("read_untouched_account");
	let untouched_address = Address::repeat_byte(0xDE);

	for depth in TRAVERSAL_DEPTHS {
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

fn bench_deep_read(c: &mut Criterion) {
	let mut group = c.benchmark_group("read_deep_account");

	for depth in TRAVERSAL_DEPTHS {
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
