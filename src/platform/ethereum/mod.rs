use {
	super::*,
	crate::{
		alloy::consensus::BlockHeader,
		reth::{
			ethereum::{evm::EthEvmConfig, node::EthereumNode},
			evm::NextBlockEnvAttributes,
			payload::builder::{
				BuildArguments,
				EthereumBuilderConfig,
				PayloadConfig,
				default_ethereum_payload,
			},
			revm::{cached::CachedReads, cancelled::CancelOnDrop},
			transaction_pool::{noop::NoopTransactionPool, *},
		},
	},
	limits::EthereumDefaultLimits,
	pool::FixedTransactions,
	serde::{Deserialize, Serialize},
	std::sync::Arc,
};

mod limits;
mod pool;

/// Platform definition for ethereum mainnet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ethereum;

impl Platform for Ethereum {
	type Bundle = FlashbotsBundle<Self>;
	type CheckpointContext = ();
	type DefaultLimits = EthereumDefaultLimits;
	type EvmConfig = EthEvmConfig;
	type ExtraLimits = ();
	type NodeTypes = EthereumNode;
	type PooledTransaction = EthPooledTransaction;

	fn evm_config<P>(chainspec: Arc<types::ChainSpec<Self>>) -> Self::EvmConfig {
		EthEvmConfig::new(chainspec)
	}

	/// Prepares a block environment containing all the information needed to
	/// build a new block. This is infallible for Ethereum and can be safely
	/// unwrapped.
	fn next_block_environment_context<P>(
		_: &types::ChainSpec<Self>,
		parent: &types::Header<Self>,
		attributes: &types::PayloadBuilderAttributes<Self>,
	) -> Result<types::NextBlockEnvContext<Self>, types::EvmEnvError<Self>> {
		Ok(NextBlockEnvAttributes {
			timestamp: attributes.timestamp,
			suggested_fee_recipient: attributes.suggested_fee_recipient,
			prev_randao: attributes.prev_randao,
			gas_limit: EthereumBuilderConfig::new().gas_limit(parent.gas_limit()),
			parent_beacon_block_root: attributes.parent_beacon_block_root,
			withdrawals: Some(attributes.withdrawals.clone()),
		})
	}

	fn build_payload<P>(
		payload: Checkpoint<P>,
		provider_factory: ProviderFactory<P>,
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

		default_ethereum_payload(
			evm_config,
			provider_factory,
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
