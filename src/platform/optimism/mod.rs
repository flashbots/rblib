use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		eips::Encodable2718,
		optimism::consensus::OpPooledTransaction as AlloyPoolTx,
		primitives::Bytes,
	},
	reth::{
		api::NodeTypes,
		chainspec::EthChainSpec,
		optimism::{
			forks::OpHardforks,
			node::{payload::builder::*, txpool::OpPooledTransaction, *},
		},
		payload::{builder::*, util::PayloadTransactionsFixed},
		primitives::Recovered,
		providers::StateProvider,
		revm::{cancelled::CancelOnDrop, database::StateProviderDatabase},
	},
	serde::{Deserialize, Serialize},
};

mod limits;
pub use limits::OptimismDefaultLimits;

/// Platform definition for Optimism Rollup chains.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Optimism;

impl Platform for Optimism {
	type Bundle = FlashbotsBundle<Self>;
	type DefaultLimits = OptimismDefaultLimits;
	type EvmConfig = OpEvmConfig;
	type NodeTypes = OpNode;
	type PooledTransaction = OpPooledTransaction;

	fn evm_config<P>(
		chainspec: std::sync::Arc<types::ChainSpec<P>>,
	) -> types::EvmConfig<P>
	where
		P: Platform<
				NodeTypes: NodeTypes<ChainSpec = types::ChainSpec<Optimism>>,
				EvmConfig = types::EvmConfig<Optimism>,
			>,
	{
		OpEvmConfig::optimism(chainspec)
	}

	fn next_block_environment_context<P>(
		chainspec: &types::ChainSpec<P>,
		parent: &types::Header<P>,
		attributes: &types::PayloadBuilderAttributes<P>,
	) -> types::NextBlockEnvContext<P>
	where
		P: traits::PlatformExecBounds<Self>,
	{
		OpNextBlockEnvAttributes {
			timestamp: attributes.payload_attributes.timestamp,
			suggested_fee_recipient: attributes
				.payload_attributes
				.suggested_fee_recipient,
			prev_randao: attributes.payload_attributes.prev_randao,
			gas_limit: attributes.gas_limit.unwrap_or(parent.gas_limit),
			parent_beacon_block_root: attributes
				.payload_attributes
				.parent_beacon_block_root,
			extra_data: if chainspec.is_holocene_active_at_timestamp(
				attributes.payload_attributes.timestamp,
			) {
				attributes
					.get_holocene_extra_data(chainspec.base_fee_params_at_timestamp(
						attributes.payload_attributes.timestamp,
					))
					.unwrap_or_default()
			} else {
				Bytes::default()
			},
		}
	}

	fn build_payload<P>(
		payload: Checkpoint<P>,
		provider: &dyn StateProvider,
	) -> Result<types::BuiltPayload<P>, PayloadBuilderError>
	where
		P: traits::PlatformExecBounds<Self>,
	{
		let block = payload.block();
		let transactions = extract_external_txs(&payload);

		let op_builder = OpBuilder::new(|_| {
			PayloadTransactionsFixed::new(
				transactions
					.into_iter()
					.map(|recovered| {
						let encoded_len = recovered.encode_2718_len();
						OpPooledTransaction::<_, AlloyPoolTx>::new(recovered, encoded_len)
					})
					.collect(),
			)
		});

		let context = OpPayloadBuilderCtx {
			evm_config: block.evm_config(),
			da_config: OpDAConfig::default(),
			chain_spec: block.chainspec().clone(),
			config: PayloadConfig::<types::PayloadBuilderAttributes<P>, _>::new(
				block.parent().clone().into(),
				(*block.attributes()).clone(),
			),
			cancel: CancelOnDrop::default(),
			best_payload: None,
		};

		// Invoke the builder implementation from reth-optimism-node.
		let build_outcome =
			op_builder.build(StateProviderDatabase(&provider), &provider, context)?;

		// extract the built payload from the build outcome.
		let built_payload = match build_outcome {
			BuildOutcomeKind::Better { payload }
			| BuildOutcomeKind::Freeze(payload) => payload,
			BuildOutcomeKind::Aborted { .. } => unreachable!(
				"We are not providing the best_payload argument to the builder."
			),
			BuildOutcomeKind::Cancelled => {
				unreachable!("CancelOnDrop is not dropped in this context.")
			}
		};

		// Done!
		Ok(built_payload)
	}
}

impl PlatformWithRpcTypes for Optimism {
	type RpcTypes = alloy::optimism::network::Optimism;
}

/// The op builder will automatically inject all transactions that are in the
/// payload attributes from the CL node. We will need to ensure that if those
/// transactions are in the transactions list, they are not duplicated and
/// removed from the transactions list provided as an argument.
///
/// This function will extract and return the list of all transactions in the
/// built payload excluding the sequencer transactions.
///
/// Payload builders might want to explicitly add those transactions during
/// the progressive payload building process to have visibility into the
/// state changes they cause and to know the cumulative gas usage including
/// those txs. This happens for example when a pipeline has a `OptimismPrologue`
/// step that applies the sequencer transactions to the payload before any other
/// step.
fn extract_external_txs<P>(
	payload: &Checkpoint<P>,
) -> Vec<Recovered<types::Transaction<Optimism>>>
where
	P: Platform<NodeTypes = types::NodeTypes<Optimism>>,
{
	let sequencer_txs = payload
		.block()
		.attributes()
		.transactions
		.iter()
		.map(|tx| tx.value().tx_hash());

	// all txs including potentially sequencer txs
	let full_history = payload.history();
	let transactions: Vec<_> = full_history.transactions().collect();

	let mut prefix_len = 0;
	for (ix, sequencer_tx) in sequencer_txs.enumerate() {
		if transactions
			.get(ix)
			.is_some_and(|tx| tx.tx_hash() == sequencer_tx)
		{
			prefix_len += 1;
		}
	}

	transactions
		.into_iter()
		.skip(prefix_len)
		.cloned()
		.collect::<Vec<_>>()
}
