//! Valgrind benchmark: state lookup cost at varying checkpoint chain depths.

#![allow(clippy::needless_pass_by_value)]

mod scenarios;

use {
	gungraun::{
		Callgrind,
		Dhat,
		EventKind,
		FlamegraphConfig,
		LibraryBenchmarkConfig,
		library_benchmark,
		library_benchmark_group,
		main,
	},
	rblib_core::{
		alloy::primitives::Address,
		payload::Checkpoint,
		platform::Ethereum,
		reth::revm::DatabaseRef,
		test_utils::FundedAccounts,
	},
	scenarios::build_checkpoint_chain,
	std::hint::black_box,
};

struct TraversalSetup {
	checkpoint: Checkpoint<Ethereum>,
	first_recipient: Address,
}

fn setup(depth: u64) -> TraversalSetup {
	let (checkpoint, first_recipient) = build_checkpoint_chain(depth);
	TraversalSetup {
		checkpoint,
		first_recipient,
	}
}

fn setup_10() -> TraversalSetup {
	setup(10)
}
fn setup_100() -> TraversalSetup {
	setup(100)
}
fn setup_500() -> TraversalSetup {
	setup(500)
}
fn setup_1000() -> TraversalSetup {
	setup(1000)
}

#[library_benchmark]
#[bench::depth_10(setup_10())]
#[bench::depth_100(setup_100())]
#[bench::depth_500(setup_500())]
#[bench::depth_1000(setup_1000())]
fn hot_read(setup: TraversalSetup) {
	let signer_address = FundedAccounts::address(0);
	black_box(setup.checkpoint.basic_ref(signer_address).unwrap());
}

#[library_benchmark]
#[bench::depth_10(setup_10())]
#[bench::depth_100(setup_100())]
#[bench::depth_500(setup_500())]
#[bench::depth_1000(setup_1000())]
fn cold_read(setup: TraversalSetup) {
	let untouched_address = Address::repeat_byte(0xDE);
	black_box(setup.checkpoint.basic_ref(untouched_address).unwrap());
}

#[library_benchmark]
#[bench::depth_10(setup_10())]
#[bench::depth_100(setup_100())]
#[bench::depth_500(setup_500())]
#[bench::depth_1000(setup_1000())]
fn deep_read(setup: TraversalSetup) {
	black_box(setup.checkpoint.basic_ref(setup.first_recipient).unwrap());
}

library_benchmark_group!(
	name = chain_traversal;
	compare_by_id = true;
	benchmarks = hot_read, cold_read, deep_read
);

main!(
	config = LibraryBenchmarkConfig::default()
		.tool(
			Callgrind::default()
				.soft_limits([(EventKind::Ir, 5.0)])
				.flamegraph(FlamegraphConfig::default())
		)
		.tool(Dhat::default());
	library_benchmark_groups = chain_traversal
);
