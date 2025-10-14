use {
	super::*,
	crate::{
		alloy::consensus::BlockHeader,
		reth::{
			ethereum::{evm::EthEvmConfig, node::EthereumNode},
			evm::NextBlockEnvAttributes,
			payload::builder::*,
			revm::{cached::CachedReads, cancelled::CancelOnDrop},
			transaction_pool::*,
		},
	},
	limits::EthereumDefaultLimits,
	pool::FixedTransactions,
	reth_transaction_pool::noop::NoopTransactionPool,
	serde::{Deserialize, Serialize},
	std::sync::Arc,
};

mod limits;
mod pool;

/// Platform definition for ethereum mainnet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ethereum;

impl Platform for Ethereum {
	type Bundle = FlashbotsBundle<Self>;
	type DefaultLimits = EthereumDefaultLimits;
	type EvmConfig = EthEvmConfig;
	type NodeTypes = EthereumNode;
	type PooledTransaction = EthPooledTransaction;

	fn evm_config<P>(chainspec: Arc<types::ChainSpec<Self>>) -> Self::EvmConfig {
		EthEvmConfig::new(chainspec)
	}

	fn next_block_environment_context<P>(
		_: &types::ChainSpec<Self>,
		parent: &types::Header<Self>,
		attributes: &types::PayloadBuilderAttributes<Self>,
	) -> types::NextBlockEnvContext<Self> {
		NextBlockEnvAttributes {
			timestamp: attributes.timestamp,
			suggested_fee_recipient: attributes.suggested_fee_recipient,
			prev_randao: attributes.prev_randao,
			gas_limit: EthereumBuilderConfig::new().gas_limit(parent.gas_limit()),
			parent_beacon_block_root: attributes.parent_beacon_block_root,
			withdrawals: Some(attributes.withdrawals.clone()),
		}
	}

	fn build_payload<P>(
		payload: Checkpoint<P>,
		provider: &dyn StateProvider,
	) -> Result<types::BuiltPayload<P>, PayloadBuilderError>
	where
		P: traits::PlatformExecBounds<Self>,
	{
		let evm_config = payload.block().evm_config().clone();
		let payload_config = PayloadConfig {
			parent_header: Arc::new(payload.block().parent().clone()),
			attributes: payload.block().attributes().clone(),
		};

		let build_args = BuildArguments::<
			types::PayloadBuilderAttributes<Self>,
			types::BuiltPayload<Self>,
		>::new(
			CachedReads::default(),
			payload_config,
			CancelOnDrop::default(),
			None,
		);

		let builder_config = EthereumBuilderConfig::new();
		let transactions = payload.history().transactions().cloned().collect();
		let transactions = Box::new(FixedTransactions::<Self>::new(transactions));

		// TODO: wrap provider and payload.block().chainspec() in a
		// StateProviderFactory + ChainSpecProvider to be able to use
		// reth_ethereum_payload_builder::default_ethereum_payload

		default_ethereum_payload(
			evm_config,
			provider,
			NoopTransactionPool::default(),
			builder_config,
			build_args,
			|_| {
				transactions
					as Box<
						dyn BestTransactions<
							Item = Arc<ValidPoolTransaction<Self::PooledTransaction>>,
						>,
					>
			},
		)?
		.into_payload()
		.ok_or_else(|| PayloadBuilderError::MissingPayload)
	}
}

impl PlatformWithRpcTypes for Ethereum {
	type RpcTypes = alloy::network::Ethereum;
}
