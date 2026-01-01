use crate::{
	prelude::*,
	reth::{
		api::FullNodeTypes,
		evm::ConfigureEvm,
		node::builder::NodeTypes,
		providers::{ChainSpecProvider, HeaderProvider, StateProviderFactory},
		transaction_pool::{PoolTransaction, TransactionPool},
	},
};

pub trait NodeBounds<P: Platform>: FullNodeTypes<Types = P::NodeTypes> {}

impl<T, P: Platform> NodeBounds<P> for T where
	T: FullNodeTypes<Types = P::NodeTypes>
{
}

/// Bounds for the provider factory required to build payloads.
pub trait ProviderFactoryBounds<P: Platform>:
	StateProviderFactory
	+ ChainSpecProvider<ChainSpec = types::ChainSpec<P>>
	+ Send
	+ Sync
{
}

impl<T, P: Platform> ProviderFactoryBounds<P> for T where
	T: StateProviderFactory
		+ ChainSpecProvider<ChainSpec = types::ChainSpec<P>>
		+ Send
		+ Sync
{
}

pub trait ProviderBounds<P: Platform>:
	ProviderFactoryBounds<P>
	+ HeaderProvider<Header = types::Header<P>>
	+ Clone
	+ 'static
{
}

impl<T, P: Platform> ProviderBounds<P> for T where
	T: ProviderFactoryBounds<P>
		+ HeaderProvider<Header = types::Header<P>>
		+ Clone
		+ 'static
{
}

pub trait PoolBounds<P: Platform>:
	TransactionPool<Transaction = P::PooledTransaction> + Unpin + 'static
{
}

impl<T, P: Platform> PoolBounds<P> for T where
	T: TransactionPool<Transaction = P::PooledTransaction> + Unpin + 'static
{
}

pub trait PooledTransactionBounds<P: Platform>:
	PoolTransaction<Consensus = types::Transaction<P>> + Send + Sync + 'static
{
}

pub trait EvmConfigBounds<P: Platform>:
	ConfigureEvm<Primitives = types::Primitives<P>> + Send + Sync
{
}

impl<T, P: Platform> EvmConfigBounds<P> for T where
	T: ConfigureEvm<Primitives = types::Primitives<P>> + Send + Sync
{
}

pub trait PlatformExecBounds<P: Platform>:
	Platform<
		NodeTypes: NodeTypes<
			ChainSpec = types::ChainSpec<P>,
			Primitives = types::Primitives<P>,
			Payload = types::PayloadTypes<P>,
		>,
		EvmConfig = types::EvmConfig<P>,
		ExtraLimits = types::ExtraLimits<P>,
	>
{
}

impl<T, P: Platform> PlatformExecBounds<T> for P where
	T: Platform<
			NodeTypes: NodeTypes<
				ChainSpec = types::ChainSpec<P>,
				Primitives = types::Primitives<P>,
				Payload = types::PayloadTypes<P>,
			>,
			EvmConfig = types::EvmConfig<P>,
			ExtraLimits = types::ExtraLimits<P>,
		>
{
}

// equal to `PlatformExecBounds` but with stricter checkpoint context
pub trait PlatformExecCtxBounds<P: Platform>:
	PlatformExecBounds<P>
	+ Platform<CheckpointContext = types::CheckpointContext<P>>
{
}

impl<T: Platform, P: Platform> PlatformExecCtxBounds<T> for P where
	P: PlatformExecBounds<T>
		+ Platform<CheckpointContext = types::CheckpointContext<T>>
{
}
