//! Valgrind benchmark: checkpoint revert vs `BundleState::revert_latest()`.

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
			db::{
				BundleState,
				WrapDatabaseRef,
				states::bundle_state::BundleRetention,
			},
		},
	},
	scenarios::{
		RevertSetup,
		revert_setup_5,
		revert_setup_25,
		revert_setup_100,
		revert_setup_250,
	},
	std::hint::black_box,
};

#[library_benchmark]
#[bench::pairs_5(revert_setup_5())]
#[bench::pairs_25(revert_setup_25())]
#[bench::pairs_100(revert_setup_100())]
#[bench::pairs_250(revert_setup_250())]
fn checkpoint(setup: RevertSetup) {
	let RevertSetup { block, pairs } = setup;
	let mut checkpoint = block.start();

	for (kept_tx, reverted_tx) in pairs {
		checkpoint = checkpoint.apply(kept_tx).unwrap();
		let _discarded = checkpoint.apply(reverted_tx).unwrap();
	}

	black_box(checkpoint);
}

#[library_benchmark]
#[bench::pairs_5(revert_setup_5())]
#[bench::pairs_25(revert_setup_25())]
#[bench::pairs_100(revert_setup_100())]
#[bench::pairs_250(revert_setup_250())]
fn direct_per_tx_merge(setup: RevertSetup) {
	let RevertSetup { block, pairs } = setup;
	let base_state = block.start();
	let evm_config = block.evm_config();
	let evm_env = block.evm_env();
	let mut bundle = BundleState::default();

	for (kept_tx, reverted_tx) in pairs {
		let mut state = State::builder()
			.with_database(WrapDatabaseRef(&base_state))
			.with_bundle_update()
			.with_bundle_prestate(bundle)
			.build();

		evm_config
			.evm_with_env(&mut state, evm_env.clone())
			.transact_commit(&kept_tx)
			.expect("tx failed");
		state.merge_transitions(BundleRetention::Reverts);

		evm_config
			.evm_with_env(&mut state, evm_env.clone())
			.transact_commit(&reverted_tx)
			.expect("tx failed");
		state.merge_transitions(BundleRetention::Reverts);

		bundle = state.take_bundle();
		bundle.revert_latest();
	}

	black_box(bundle);
}

#[library_benchmark]
#[bench::pairs_5(revert_setup_5())]
#[bench::pairs_25(revert_setup_25())]
#[bench::pairs_100(revert_setup_100())]
#[bench::pairs_250(revert_setup_250())]
fn direct_batch_baseline(setup: RevertSetup) {
	let RevertSetup { block, pairs } = setup;
	let base_state = block.start();

	let mut state = State::builder()
		.with_database(WrapDatabaseRef(base_state))
		.with_bundle_update()
		.build();

	let evm_config = block.evm_config();
	let evm_env = block.evm_env();

	for (kept_tx, _) in pairs {
		evm_config
			.evm_with_env(&mut state, evm_env.clone())
			.transact_commit(&kept_tx)
			.expect("tx failed");
	}

	state.merge_transitions(BundleRetention::Reverts);
	black_box(state.take_bundle());
}

library_benchmark_group!(
	name = apply_revert_continue;
	compare_by_id = true;
	benchmarks = checkpoint, direct_per_tx_merge, direct_batch_baseline
);

main!(
	config = LibraryBenchmarkConfig::default()
		.tool(
			Callgrind::default()
				.soft_limits([(EventKind::Ir, 5.0)])
				.flamegraph(FlamegraphConfig::default())
		)
		.tool(Dhat::default());
	library_benchmark_groups = apply_revert_continue
);
