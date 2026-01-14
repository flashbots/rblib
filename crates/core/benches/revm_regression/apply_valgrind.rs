//! Valgrind benchmark: `checkpoint.apply()` vs direct `transact_commit` on
//! single State.

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
	rblib_core::reth::{
		evm::{ConfigureEvm, Evm},
		revm::{
			State,
			db::{WrapDatabaseRef, states::bundle_state::BundleRetention},
		},
	},
	scenarios::{
		ApplySetup,
		apply_setup_10,
		apply_setup_100,
		apply_setup_500,
		apply_setup_1000,
	},
	std::hint::black_box,
};

#[library_benchmark]
#[bench::tx_10(apply_setup_10())]
#[bench::tx_100(apply_setup_100())]
#[bench::tx_500(apply_setup_500())]
#[bench::tx_1000(apply_setup_1000())]
fn checkpoint_chain(setup: ApplySetup) {
	let ApplySetup { block, txs } = setup;
	let mut checkpoint = block.start();

	for tx in txs {
		checkpoint = checkpoint.apply(tx).unwrap();
	}

	black_box(checkpoint);
}

#[library_benchmark]
#[bench::tx_10(apply_setup_10())]
#[bench::tx_100(apply_setup_100())]
#[bench::tx_500(apply_setup_500())]
#[bench::tx_1000(apply_setup_1000())]
fn single_state(setup: ApplySetup) {
	let ApplySetup { block, txs } = setup;
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
			.expect("tx failed");
	}

	state.merge_transitions(BundleRetention::Reverts);
	black_box(state.take_bundle());
}

library_benchmark_group!(
	name = tx_execution;
	compare_by_id = true;
	benchmarks = checkpoint_chain, single_state
);

main!(
	config = LibraryBenchmarkConfig::default()
		.tool(
			Callgrind::default()
				.soft_limits([(EventKind::Ir, 5.0)])
				.flamegraph(FlamegraphConfig::default())
		)
		.tool(Dhat::default());
	library_benchmark_groups = tx_execution
);
