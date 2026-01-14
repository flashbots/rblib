//! Criterion benchmark: checkpoint revert vs `BundleState::revert_latest()`.

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
			db::{
				BundleState,
				WrapDatabaseRef,
				states::bundle_state::BundleRetention,
			},
		},
	},
	scenarios::{
		REVERT_PAIR_COUNTS,
		generate_transaction_pairs,
		generate_transactions,
	},
};

fn bench_apply_revert_continue(c: &mut Criterion) {
	let mut group = c.benchmark_group("apply_revert_continue");

	for num_pairs in REVERT_PAIR_COUNTS {
		let pairs = generate_transaction_pairs(num_pairs);
		let param = format!("{num_pairs}_pairs");

		group.throughput(Throughput::Elements(num_pairs));

		group.bench_with_input(
			BenchmarkId::new("checkpoint", &param),
			&pairs,
			|b, pairs| {
				b.iter_with_setup(
					|| {
						let setup = scenarios::revert_setup(num_pairs);
						let checkpoint = setup.block.start();
						(setup.block, checkpoint, pairs.clone())
					},
					|(_block, mut checkpoint, pairs)| {
						for (kept_tx, reverted_tx) in pairs {
							checkpoint = checkpoint.apply(kept_tx).unwrap();
							let _discarded = checkpoint.apply(reverted_tx).unwrap();
						}
						checkpoint
					},
				);
			},
		);

		group.bench_with_input(
			BenchmarkId::new("direct_per_tx_merge", &param),
			&pairs,
			|b, pairs| {
				b.iter_with_setup(
					|| {
						let setup = scenarios::revert_setup(num_pairs);
						(setup.block, pairs.clone())
					},
					|(block, pairs)| {
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

						bundle
					},
				);
			},
		);

		group.bench_with_input(
			BenchmarkId::new("direct_batch_baseline", &param),
			&pairs,
			|b, pairs| {
				b.iter_with_setup(
					|| {
						let setup = scenarios::revert_setup(num_pairs);
						let kept_txs: Vec<_> =
							pairs.iter().map(|(kept, _)| kept.clone()).collect();
						(setup.block, kept_txs)
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

fn bench_apply_then_revert(c: &mut Criterion) {
	let mut group = c.benchmark_group("apply_then_revert");

	let test_cases: [(u64, u64); 4] = [(10, 5), (50, 25), (100, 10), (100, 50)];

	for (total, to_revert) in test_cases {
		let txs = generate_transactions(total);
		let param = format!("{total}_apply_{to_revert}_revert");
		let kept = total - to_revert;

		group.throughput(Throughput::Elements(kept));

		group.bench_with_input(
			BenchmarkId::new("checkpoint", &param),
			&txs,
			|b, txs| {
				b.iter_with_setup(
					|| {
						let setup = scenarios::apply_setup(total);
						let checkpoint = setup.block.start();
						(
							setup.block,
							checkpoint,
							txs.clone(),
							usize::try_from(to_revert).unwrap(),
						)
					},
					|(_block, mut checkpoint, txs, to_revert)| {
						let mut checkpoints = Vec::with_capacity(txs.len());

						for tx in txs {
							checkpoint = checkpoint.apply(tx).unwrap();
							checkpoints.push(checkpoint.clone());
						}

						let revert_to_idx = checkpoints.len() - to_revert - 1;
						checkpoints[revert_to_idx].clone()
					},
				);
			},
		);

		group.bench_with_input(
			BenchmarkId::new("direct_per_tx_merge", &param),
			&txs,
			|b, txs| {
				b.iter_with_setup(
					|| {
						let setup = scenarios::apply_setup(total);
						(
							setup.block,
							txs.clone(),
							usize::try_from(to_revert).unwrap(),
						)
					},
					|(block, txs, to_revert)| {
						let base_state = block.start();
						let evm_config = block.evm_config();
						let evm_env = block.evm_env();
						let mut bundle = BundleState::default();

						for tx in &txs {
							let mut state = State::builder()
								.with_database(WrapDatabaseRef(&base_state))
								.with_bundle_update()
								.with_bundle_prestate(bundle)
								.build();

							evm_config
								.evm_with_env(&mut state, evm_env.clone())
								.transact_commit(tx)
								.expect("tx failed");

							state.merge_transitions(BundleRetention::Reverts);
							bundle = state.take_bundle();
						}

						bundle.revert(to_revert);
						bundle
					},
				);
			},
		);
	}

	group.finish();
}

criterion_group!(
	benches,
	bench_apply_revert_continue,
	bench_apply_then_revert
);
criterion_main!(benches);
