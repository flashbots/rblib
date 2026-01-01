use {
	super::*,
	crate::{alloy, prelude::*, reth},
	alloy::{
		eips::{BlockNumberOrTag, eip7685::RequestsOrHash},
		primitives::B256,
		providers::Provider,
		rpc::types::Block,
	},
	reth::{
		chainspec,
		ethereum::node::{
			EthEngineTypes,
			EthereumNode,
			engine::EthPayloadAttributes,
			node::EthereumAddOns,
		},
		payload::builder::PayloadId,
		rpc::{api::EngineApiClient, types::engine::ForkchoiceState},
	},
	reth_ipc::client::IpcClientBuilder,
};

impl TestNodeFactory<Ethereum> for Ethereum {
	type CliExtArgs = ();
	type ConsensusDriver = EthConsensusDriver;

	async fn create_test_node_with_args(
		pipeline: Pipeline<Ethereum>,
		(): Self::CliExtArgs,
	) -> eyre::Result<LocalNode<Ethereum, Self::ConsensusDriver>> {
		let chainspec = chainspec::DEV.as_ref().clone().with_funded_accounts();
		LocalNode::new(EthConsensusDriver, chainspec, move |builder| {
			builder
				.with_types::<EthereumNode>()
				.with_components(
					EthereumNode::components().payload(pipeline.into_service()),
				)
				.with_add_ons(EthereumAddOns::default())
		})
		.await
	}
}

pub struct EthConsensusDriver;
impl ConsensusDriver<Ethereum> for EthConsensusDriver {
	type Params = ();

	async fn start_building(
		&self,
		node: &LocalNode<Ethereum, Self>,
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

		let payload_attributes = EthPayloadAttributes {
			timestamp: target_timestamp,
			prev_randao: B256::random(),
			suggested_fee_recipient: TEST_COINBASE,
			withdrawals: Some(vec![]),
			parent_beacon_block_root: Some(B256::ZERO),
		};

		// Start the production of a new block
		let fcu_result = EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
			&ipc_client,
			ForkchoiceState {
				head_block_hash: latest_block.header.hash,
				safe_block_hash: latest_block.header.hash,
				finalized_block_hash: latest_block.header.hash,
			},
			Some(payload_attributes),
		)
		.await?;

		if fcu_result.is_invalid() {
			return Err(eyre::eyre!("Forkchoice update failed: {fcu_result:#?}"));
		}

		Ok(fcu_result.payload_id.expect(
			"validated that it is a valid result and should have a payload ID",
		))
	}

	async fn finish_building(
		&self,
		node: &LocalNode<Ethereum, Self>,
		payload_id: PayloadId,
		(): &Self::Params,
	) -> eyre::Result<Block> {
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

		// Retrieve the payload using the payload ID
		let getpayload_result = EngineApiClient::<EthEngineTypes>::get_payload_v4(
			&ipc_client,
			payload_id,
		)
		.await?;

		let payload = getpayload_result.execution_payload.clone();
		let new_block_hash = payload.payload_inner.payload_inner.block_hash;

		// Give the newly built payload to the EL node and let it validate it.
		let new_payload_result = EngineApiClient::<EthEngineTypes>::new_payload_v4(
			&ipc_client,
			payload,
			vec![],
			B256::ZERO,
			RequestsOrHash::default(),
		)
		.await?;

		if new_payload_result.is_invalid() {
			return Err(eyre::eyre!(
				"Failed to set new payload: {new_payload_result:#?}"
			));
		}

		// update the canonical chain with the new block without triggering new
		// payload production
		let fcu_result = EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
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
