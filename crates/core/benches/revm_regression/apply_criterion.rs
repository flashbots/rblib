//! Criterion benchmark: `checkpoint.apply()` vs direct `transact_commit` on
//! single State.

mod scenarios;

use {
	criterion::{
		BenchmarkId,
		Criterion,
		Throughput,
		criterion_group,
		criterion_main,
	},
	rblib_core::reth::{
		evm::{ConfigureEvm, Evm},
		revm::{
			State,
			db::{WrapDatabaseRef, states::bundle_state::BundleRetention},
		},
	},
	scenarios::{APPLY_SAMPLES, ApplySetup, generate_transactions},
};

fn bench_checkpoint_vs_direct(c: &mut Criterion) {
	let mut group = c.benchmark_group("tx_execution");

	for n in APPLY_SAMPLES {
		let txs = generate_transactions(n);
		let param = format!("{n}_transfers");

		group.throughput(Throughput::Elements(n));

		group.bench_with_input(
			BenchmarkId::new("checkpoint_chain", &param),
			&txs,
			|b, txs| {
				b.iter_with_setup(
					|| {
						let setup = scenarios::apply_setup(n);
						let checkpoint = setup.block.start();
						(setup.block, checkpoint, txs.clone())
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

		group.bench_with_input(
			BenchmarkId::new("single_state", &param),
			&txs,
			|b, txs| {
				b.iter_with_setup(
					|| {
						let ApplySetup { block, .. } = scenarios::apply_setup(n);
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
								.expect("tx failed");
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
