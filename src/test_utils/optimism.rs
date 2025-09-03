use {
	super::*,
	crate::{
		alloy,
		pool::{HostNodeInstaller, OrderPool},
		prelude::*,
		reth,
	},
	alloy::{
		eips::{BlockNumberOrTag, eip7685::Requests},
		optimism::{
			network::{BlockResponse, Optimism as AlloyOpNetwork},
			rpc_types::Transaction,
		},
		primitives::B256,
		providers::Provider,
	},
	reth::{
		ethereum::node::engine::EthPayloadAttributes as PayloadAttributes,
		node::builder::Node,
		optimism::{
			chainspec::{
				self,
				constants::{
					BASE_MAINNET_MAX_GAS_LIMIT,
					TX_SET_L1_BLOCK_OP_MAINNET_BLOCK_124665056,
				},
			},
			node::{
				OpEngineApiBuilder,
				OpEngineTypes,
				OpEngineValidatorBuilder,
				OpNode,
				OpPayloadAttributes,
				args::RollupArgs,
			},
			rpc::OpEngineApiClient,
		},
		payload::builder::PayloadId,
		rpc::types::{Block, engine::ForkchoiceState},
	},
	reth_ipc::client::IpcClientBuilder,
};

impl TestNodeFactory<Optimism> for Optimism {
	type CliExtArgs = RollupArgs;
	type ConsensusDriver = OptimismConsensusDriver;

	async fn create_test_node_with_args_and_pool(
		pipeline: Pipeline<Optimism>,
		args: Self::CliExtArgs,
		pool: Option<OrderPool<Optimism>>,
	) -> eyre::Result<LocalNode<Optimism, Self::ConsensusDriver>> {
		let chainspec = chainspec::OP_DEV.as_ref().clone().with_funded_accounts();
		if let Some(pool) = pool {
			LocalNode::new(OptimismConsensusDriver, chainspec, move |builder| {
				let opnode = OpNode::new(args);
				builder
					.with_types::<OpNode>()
					.with_components(
						opnode
							.components()
							.replace_pool(&pool)
							.payload(pipeline.into_service()),
					)
					.with_add_ons(opnode
						.add_ons_builder::<types::RpcTypes<Optimism>>()
						.build::<_, OpEngineValidatorBuilder, OpEngineApiBuilder<OpEngineValidatorBuilder>>())
					.extend_rpc_modules(move |mut rpc_ctx| pool.attach_rpc(&mut rpc_ctx))
			})
			.await
		} else {
			LocalNode::new(OptimismConsensusDriver, chainspec, move |builder| {
				let opnode = OpNode::new(args);
				builder
					.with_types::<OpNode>()
					.with_components(opnode.components().payload(pipeline.into_service()))
					.with_add_ons(opnode.add_ons())
			})
			.await
		}
	}
}

pub struct OptimismConsensusDriver;
impl<P> ConsensusDriver<P> for OptimismConsensusDriver
where
	P: traits::PlatformExecBounds<Optimism>
		+ PlatformWithRpcTypes<RpcTypes = AlloyOpNetwork>,
{
	type Params = ();

	async fn start_building(
		&self,
		node: &LocalNode<P, Self>,
		target_timestamp: u64,
		(): &Self::Params,
	) -> eyre::Result<PayloadId> {
		let ipc_path = node.config().rpc.auth_ipc_path.clone();
		let ipc_client = IpcClientBuilder::default()
			.build(&ipc_path)
			.await
			.expect("Failed to create ipc client");

		let latest_block = node
			.provider()
			.get_block_by_number(BlockNumberOrTag::Latest)
			.await?
			.expect("Latest block should exist");

		let payload_attributes = OpPayloadAttributes {
			payload_attributes: PayloadAttributes {
				timestamp: target_timestamp,
				parent_beacon_block_root: Some(B256::ZERO),
				suggested_fee_recipient: TEST_COINBASE,
				withdrawals: Some(vec![]),
				..Default::default()
			},
			transactions: Some(vec![
				TX_SET_L1_BLOCK_OP_MAINNET_BLOCK_124665056.into(),
			]),
			gas_limit: Some(BASE_MAINNET_MAX_GAS_LIMIT),
			..Default::default()
		};

		let fcu_result =
			OpEngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
				&ipc_client,
				ForkchoiceState {
					head_block_hash: latest_block.header().hash,
					safe_block_hash: latest_block.header().hash,
					finalized_block_hash: latest_block.header().hash,
				},
				Some(payload_attributes),
			)
			.await?;

		if fcu_result.payload_status.is_invalid() {
			return Err(eyre::eyre!("Forkchoice update failed: {fcu_result:?}"));
		}

		fcu_result.payload_id.ok_or_else(|| {
			eyre::eyre!("Forkchoice update did not return a payload ID")
		})
	}

	async fn finish_building(
		&self,
		node: &LocalNode<P, Self>,
		payload_id: PayloadId,
		(): &Self::Params,
	) -> eyre::Result<Block<Transaction>> {
		let ipc_path = node.config().rpc.auth_ipc_path.clone();
		let ipc_client = IpcClientBuilder::default()
			.build(&ipc_path)
			.await
			.expect("Failed to create ipc client");

		let latest_block = node
			.provider()
			.get_block_by_number(BlockNumberOrTag::Latest)
			.await?
			.expect("Latest block should exist");

		let getpayload_result = OpEngineApiClient::<OpEngineTypes>::get_payload_v4(
			&ipc_client,
			payload_id,
		)
		.await?;

		let payload = getpayload_result.execution_payload;
		let new_block_hash =
			payload.payload_inner.payload_inner.payload_inner.block_hash;

		// Give the newly built payload to the EL node and let it validate it.
		let new_payload_result =
			OpEngineApiClient::<OpEngineTypes>::new_payload_v4(
				&ipc_client,
				payload,
				vec![],
				B256::ZERO,
				Requests::default(),
			)
			.await?;

		if new_payload_result.is_invalid() {
			return Err(eyre::eyre!(
				"Failed to set new payload: {new_payload_result:#?}"
			));
		}

		let fcu_result =
			OpEngineApiClient::<OpEngineTypes>::fork_choice_updated_v3(
				&ipc_client,
				ForkchoiceState {
					head_block_hash: new_block_hash,
					safe_block_hash: latest_block.header.hash,
					finalized_block_hash: latest_block.header.hash,
				},
				None,
			)
			.await?;

		if fcu_result.is_invalid() {
			return Err(eyre::eyre!("Forkchoice update failed: {fcu_result:#?}"));
		}

		let block = node
			.provider()
			.get_block_by_number(BlockNumberOrTag::Latest)
			.full()
			.await?
			.expect("New block should exist");

		assert_eq!(
			block.header.hash, new_block_hash,
			"New block hash should match the one returned by the payload"
		);

		Ok(block)
	}
}
