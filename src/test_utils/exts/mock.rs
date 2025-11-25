//! Extensions for creating mocked versions of various API primitives that
//! typically have non-trivial construction process.

use {
	super::*,
	alloy::primitives::{Address, B256},
	reth::{
		chainspec::EthChainSpec,
		ethereum::{
			node::engine::EthPayloadAttributes as PayloadAttributes,
			primitives::AlloyBlockHeader,
		},
		payload::builder::EthPayloadBuilderAttributes,
		primitives::SealedHeader,
	},
	std::sync::{Arc, LazyLock},
};

/// A helper trait to provide a default testnet for a platform
pub trait PlatformWithTestnet: Platform {
	fn dev_chainspec() -> Arc<types::ChainSpec<Self>>;
}

/// Blanket implementation for ethereum
impl PlatformWithTestnet for Ethereum {
	fn dev_chainspec() -> Arc<types::ChainSpec<Self>> {
		LazyLock::force(&crate::reth::chainspec::DEV)
			.clone()
			.with_funded_accounts()
	}
}

/// Blanket implementation for optimism
#[cfg(feature = "optimism")]
impl PlatformWithTestnet for Optimism {
	fn dev_chainspec() -> Arc<types::ChainSpec<Self>> {
		LazyLock::force(&crate::reth::optimism::chainspec::OP_DEV)
			.clone()
			.with_funded_accounts()
	}
}

/// Payload builder attributes typically come from the Consensus Layer with a
/// forkchoiceUpdate request, and are typically obtained from instances of types
/// that implement [`PayloadJobGenerator`] and are configured as payload
/// builders.
///
/// See [`crate::pipelines::service::PipelineServiceBuilder`] for an example of
/// a workflow that creates a payload builder attributes instance in real world
/// settings.
pub trait PayloadBuilderAttributesMocked<P: Platform> {
	fn mocked(parent: &SealedHeader<types::Header<P>>) -> Self;
}

fn default_eth_payload_builder_attributed<P: Platform>(
	parent: &SealedHeader<types::Header<P>>,
) -> EthPayloadBuilderAttributes {
	EthPayloadBuilderAttributes::new(parent.hash(), PayloadAttributes {
		timestamp: parent.header().timestamp() + 1,
		prev_randao: B256::random(),
		suggested_fee_recipient: Address::random(),
		withdrawals: Some(vec![]),
		parent_beacon_block_root: Some(B256::ZERO),
	})
}

impl<P: Platform> PayloadBuilderAttributesMocked<P>
	for EthPayloadBuilderAttributes
{
	fn mocked(parent: &SealedHeader<types::Header<P>>) -> Self {
		default_eth_payload_builder_attributed::<P>(parent)
	}
}

#[cfg(feature = "optimism")]
impl<P: Platform> PayloadBuilderAttributesMocked<P>
	for reth::optimism::node::OpPayloadBuilderAttributes<types::Transaction<P>>
{
	fn mocked(parent: &SealedHeader<types::Header<P>>) -> Self {
		use {
			alloy::eips::eip2718::Decodable2718,
			reth::optimism::{
				chainspec::constants::{
					BASE_MAINNET_MAX_GAS_LIMIT,
					TX_SET_L1_BLOCK_OP_MAINNET_BLOCK_124665056,
				},
				node::OpPayloadBuilderAttributes,
			},
			reth_ethereum::primitives::WithEncoded,
		};

		let sequencer_tx = WithEncoded::new(
			TX_SET_L1_BLOCK_OP_MAINNET_BLOCK_124665056.into(),
			types::Transaction::<P>::decode_2718(
				&mut &TX_SET_L1_BLOCK_OP_MAINNET_BLOCK_124665056[..],
			)
			.expect("Failed to decode test transaction"),
		);

		OpPayloadBuilderAttributes {
			payload_attributes: default_eth_payload_builder_attributed::<P>(parent),
			transactions: vec![sequencer_tx],
			gas_limit: Some(BASE_MAINNET_MAX_GAS_LIMIT),
			..Default::default()
		}
	}
}

/// Allows the creation of a block context for the first block post genesis
pub trait BlockContextMocked<P: Platform> {
	/// Returns an instance of a [`BlockContext`] that is rooted at the genesis
	/// block the given platform's DEV chainspec.
	///
	/// The base state used in the [`BlockContext`] is platform-specific and
	/// pre-populated with [`FundedAccounts`] for the given platform. This
	/// [`BlockContext`] instance together with a checkpoint created on top of it
	/// can be used to construct a payload using [`Platform::build_payload`] with
	/// [`BlockContext::build_payload`].
	fn mocked() -> BlockContext<P>;
}

impl<P> BlockContextMocked<P> for BlockContext<P>
where
	P: PlatformWithTestnet,
	types::PayloadBuilderAttributes<P>: PayloadBuilderAttributesMocked<P>,
{
	fn mocked() -> BlockContext<P> {
		let chainspec = P::dev_chainspec();
		let provider_factory = GenesisProviderFactory::<P>::new(chainspec.clone());
		let base_state = provider_factory.state_provider();

		let parent = SealedHeader::new(
			chainspec.genesis_header().clone(),
			chainspec.genesis_hash(),
		);

		let payload_attributes =
			<types::PayloadBuilderAttributes<P> as PayloadBuilderAttributesMocked<
				P,
			>>::mocked(&parent);

		BlockContext::<P>::new(
			parent,
			payload_attributes,
			base_state,
			chainspec.clone(),
			None,
		)
		.expect("Failed to create mocked block context")
	}
}
