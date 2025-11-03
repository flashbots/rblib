//! Example of creating a basic block builder for OP stack chains

use {
	rblib::{
		pool::{AppendOrders, HostNodeInstaller, OrderPool},
		prelude::*,
		steps::{OptimismPrologue, OrderByPriorityFee},
	},
	reth_optimism_cli::Cli,
	reth_optimism_node::{
		OpAddOns,
		OpEngineApiBuilder,
		OpEngineValidatorBuilder,
		OpNode,
	},
	reth_optimism_rpc::OpEthApiBuilder,
};

/// Basic block builder
///
/// Block building strategy that builds blocks using the classic approach by
/// prepending sequencer transactions, then ordering the rest of the
/// transactions by tip.
fn build_basic_pipeline(pool: &OrderPool<Optimism>) -> Pipeline<Optimism> {
	let pipeline = Pipeline::<Optimism>::named("classic")
		.with_prologue(OptimismPrologue)
		.with_pipeline(
			Loop,
			(AppendOrders::from_pool(pool), OrderByPriorityFee::default()),
		);

	pool.attach_pipeline(&pipeline);

	pipeline
}

fn main() {
	Cli::parse_args()
		.run(|builder, _cli_args| async move {
			let pool = OrderPool::<Optimism>::default();
			let pipeline = build_basic_pipeline(&pool);
			let op_node = OpNode::default();

			let add_ons: OpAddOns<
				_,
				OpEthApiBuilder,
				OpEngineValidatorBuilder,
				OpEngineApiBuilder<OpEngineValidatorBuilder>,
			> = op_node
				.add_ons_builder::<types::RpcTypes<Optimism>>()
				.build();

			let handle = builder
				.with_types::<OpNode>()
				.with_components(
					op_node
						.components()
						.attach_pool(&pool)
						.payload(pipeline.into_service()),
				)
				.with_add_ons(add_ons)
				.launch()
				.await?;

			handle.wait_for_node_exit().await
		})
		.unwrap();
}
