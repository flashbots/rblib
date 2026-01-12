//! Criterion benchmark: cost of creating and using parallel branches from a
//! shared checkpoint.

mod scenarios;

use {
	criterion::{BenchmarkId, Criterion, criterion_group, criterion_main},
	rblib_core::reth::primitives::Recovered,
	scenarios::{
		BRANCH_COUNTS,
		FORK_DEPTHS,
		Transaction,
		build_base_chain,
		generate_branch_txs,
	},
};

fn bench_fork_and_apply(c: &mut Criterion) {
	let mut group = c.benchmark_group("fork_and_apply");

	for depth in FORK_DEPTHS {
		for num_branches in BRANCH_COUNTS {
			let branch_txs: Vec<Vec<Recovered<Transaction>>> = (1..=num_branches)
				.map(|signer_idx| {
					generate_branch_txs(u32::try_from(signer_idx).unwrap())
				})
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

						for txs in *branch_txs {
							let mut branch = (*fork_point).clone();

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
