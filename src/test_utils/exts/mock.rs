//! Extensions for creating mocked versions of various API primitives that
//! typically have non-trivial construction process.

use {
	super::*,
	alloy::primitives::{Address, B256},
	reth::{
		chainspec::{DEV, EthChainSpec},
		ethereum::{
			node::engine::EthPayloadAttributes as PayloadAttributes,
			primitives::AlloyBlockHeader,
		},
		node::builder::NodeTypes,
		payload::builder::EthPayloadBuilderAttributes,
		primitives::SealedHeader,
	},
	std::sync::LazyLock,
};

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

impl<P: Platform> PayloadBuilderAttributesMocked<P>
	for EthPayloadBuilderAttributes
{
	fn mocked(parent: &SealedHeader<types::Header<P>>) -> Self {
		EthPayloadBuilderAttributes::new(parent.hash(), PayloadAttributes {
			timestamp: parent.header().timestamp() + 1,
			prev_randao: B256::random(),
			suggested_fee_recipient: Address::random(),
			withdrawals: Some(vec![]),
			parent_beacon_block_root: Some(B256::ZERO),
		})
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
			payload_attributes: EthPayloadBuilderAttributes::new(
				parent.hash(),
				PayloadAttributes {
					timestamp: parent.header().timestamp() + 1,
					prev_randao: B256::random(),
					suggested_fee_recipient: Address::random(),
					withdrawals: Some(vec![]),
					parent_beacon_block_root: Some(B256::ZERO),
				},
			),
			transactions: vec![sequencer_tx],
			gas_limit: Some(BASE_MAINNET_MAX_GAS_LIMIT),
			..Default::default()
		}
	}
}

/// Allows the creation of a block context for the first block post genesis with
/// all [`FundedAccounts`] pre-funded with 100 ETH.
pub trait BlockContextMocked<P: Platform, Marker = ()> {
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

impl<P: Platform> BlockContextMocked<P, Variant<0>> for BlockContext<P>
where
	P: Platform<
		NodeTypes: NodeTypes<
			ChainSpec = types::ChainSpec<Ethereum>,
			Primitives = types::Primitives<Ethereum>,
			Payload = types::PayloadTypes<Ethereum>,
		>,
	>,
{
	fn mocked() -> BlockContext<P> {
		let chainspec = LazyLock::force(&DEV).clone().with_funded_accounts();
		let provider_factory = GenesisProviderFactory::<P>::new(chainspec.clone());
		let base_state = provider_factory.state_provider();

		let parent = SealedHeader::new(
			chainspec.genesis_header().clone(),
			chainspec.genesis_hash(),
		);

		let payload_attribs = <EthPayloadBuilderAttributes as PayloadBuilderAttributesMocked<P>>::mocked(&parent);

		BlockContext::<P>::new(
			parent,
			payload_attribs,
			base_state,
			chainspec.clone(),
			None,
		)
		.expect("Failed to create mocked block context")
	}
}

#[cfg(feature = "optimism")]
impl<P: Platform> BlockContextMocked<P, Variant<1>> for BlockContext<P>
where
	P: Platform<
		NodeTypes: NodeTypes<
			ChainSpec = types::ChainSpec<Optimism>,
			Primitives = types::Primitives<Optimism>,
			Payload = types::PayloadTypes<Optimism>,
		>,
	>,
{
	fn mocked() -> BlockContext<P> {
		use reth::optimism::{chainspec::OP_DEV, node::OpPayloadBuilderAttributes};
		let chainspec = LazyLock::force(&OP_DEV).clone().with_funded_accounts();
		let provider_factory = GenesisProviderFactory::<P>::new(chainspec.clone());
		let base_state = provider_factory.state_provider();

		let parent = SealedHeader::new(
			chainspec.genesis_header().clone(),
			chainspec.genesis_hash(),
		);

		let payload_attributes =
			<OpPayloadBuilderAttributes<types::Transaction<P>> as PayloadBuilderAttributesMocked<P>>::mocked(
				&parent,
			);

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
