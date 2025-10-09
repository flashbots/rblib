//! Helpers and utilities for extracting types from the platform definition.

use {
	super::*,
	reth::revm::context::result::ExecutionResult,
	reth_evm::EvmFactoryFor,
};

/// Extracts the type that configures the essential types of the platform.
pub type NodeTypes<P: Platform> = P::NodeTypes;

/// Extracts node's engine API types used when interacting with CL.
pub type PayloadTypes<P: Platform> =
	<NodeTypes<P> as reth::api::NodeTypes>::Payload;

/// Extracts the node's primitive types that define transactions, blocks,
/// headers, etc.
pub type Primitives<P: Platform> =
	<BuiltPayload<P> as reth::api::BuiltPayload>::Primitives;

/// Extracts the concrete block type from the platform definition.
pub type Block<P: Platform> =
	<Primitives<P> as reth::api::NodePrimitives>::Block;

/// Extracts the receipt type from the platform definition.
pub type Receipt<P: Platform> =
	<Primitives<P> as reth::api::NodePrimitives>::Receipt;

/// Extracts the block header type from the platform definition.
pub type Header<P: Platform> = <Block<P> as reth::api::Block>::Header;

/// Extracts the platform-specific payload attributes type that comes from the
/// CL node.
pub type PayloadAttributes<P: Platform> =
	<PayloadTypes<P> as reth::api::PayloadTypes>::PayloadAttributes;

/// Extracts the type used internally during payload building  when the CL
/// node requests a new payload to be built by the EL node.
pub type PayloadBuilderAttributes<P: Platform> =
	<PayloadTypes<P> as reth::api::PayloadTypes>::PayloadBuilderAttributes;

/// Extracts the transaction type for this platform.
pub type Transaction<P: Platform> =
	<Primitives<P> as reth::api::NodePrimitives>::SignedTx;

/// Extracts the type that represents a transaction that is in the transaction
/// pool.
pub type PooledTransaction<P: Platform> = P::PooledTransaction;

/// Extracts the type that represents an atomic bundle of transactions.
pub type Bundle<P: Platform> = P::Bundle;

/// Extracts the type that represent the optional post-execution bundle
/// validation error.
pub type BundlePostExecutionError<P: Platform> =
	<Bundle<P> as super::Bundle<P>>::PostExecutionError;

/// Extracts the type that represents the final outcome of a payload building
/// process, which is a built payload that can be submitted to the consensus
/// engine.
pub type BuiltPayload<P: Platform> =
	<PayloadTypes<P> as reth::api::PayloadTypes>::BuiltPayload;

/// Extracts the type that holds chain-specific information needed to build a
/// block that cannot be deduced automatically from the parent header.
pub type NextBlockEnvContext<P: Platform> =
	<EvmConfig<P> as reth::api::ConfigureEvm>::NextBlockEnvCtx;

/// Extracts the type that represents the EVM environment for this platform.
pub type EvmEnv<P: Platform> = reth::evm::EvmEnvFor<EvmConfig<P>>;

/// Extracts the type that holds the EVM configuration for this platform.
pub type EvmConfig<P: Platform> = P::EvmConfig;

/// Extracts the error type used by the EVM environment configuration when
/// creating evm environment for a new block. See
/// [`reth::api::ConfigureEvm::next_evm_env`].
pub type EvmEnvError<P: Platform> =
	<EvmConfig<P> as reth::api::ConfigureEvm>::Error;

/// Extracts the `ChainSpec` type from the platform definition.
pub type ChainSpec<P: Platform> =
	<P::NodeTypes as reth::api::NodeTypes>::ChainSpec;

/// Extracts the type that is responsible for creating instances of the EVM
/// for this platform.
pub type EvmFactory<P: Platform> = EvmFactoryFor<P::EvmConfig>;

/// Extracts the type returned by EVM. Contains either errors related to
/// invalid transactions or internal irrecoverable execution errors.
pub type EvmError<P: Platform, DBError> =
	<EvmFactory<P> as reth::evm::EvmFactory>::Error<DBError>;

/// Extracts the type that describes the reason why EVM execution of a
/// transaction was halted.
pub type EvmHaltReason<P: Platform> =
	<EvmFactory<P> as reth::evm::EvmFactory>::HaltReason;

/// Extracts the type that can provide limits for the payload building
/// process.
pub type DefaultLimits<P: Platform> = P::DefaultLimits;

/// The result of executing a transaction in the EVM.
pub type TransactionExecutionResult<P: Platform> =
	ExecutionResult<EvmHaltReason<P>>;

/// For platforms that implement the `PlatformWithRpcTypes` trait, this type
/// extracts the top-level alloy network types aggregate type.
pub type RpcTypes<P: PlatformWithRpcTypes> =
	<P as PlatformWithRpcTypes>::RpcTypes;

/// Extracts the type that represents a transaction receipt over RPC.
pub type ReceiptResponse<P: PlatformWithRpcTypes> =
	<RpcTypes<P> as AlloyNetwork>::ReceiptResponse;

/// Extracts the type that allows interactions with RPC calls that return
/// blocks.
pub type BlockResponse<P: PlatformWithRpcTypes> =
	<RpcTypes<P> as AlloyNetwork>::BlockResponse;

/// Extracts the type that allows building transactions for the platform
/// using the alloy-rs utilities.
pub type TransactionRequest<P: PlatformWithRpcTypes> =
	<RpcTypes<P> as AlloyNetwork>::TransactionRequest;

/// Extracts the EIP-2718 transaction envelope type for the platform.
/// This type is convertible to `types::transaction<P>`.
pub type TxEnvelope<P: PlatformWithRpcTypes> =
	<RpcTypes<P> as AlloyNetwork>::TxEnvelope;

pub type UnsignedTx<P: PlatformWithRpcTypes> =
	<RpcTypes<P> as AlloyNetwork>::UnsignedTx;
