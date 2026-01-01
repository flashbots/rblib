use {
	crate::{alloy, prelude::*, reth},
	alloy::consensus::BlockHeader,
	core::time::Duration,
	reth::{api::NodeTypes, chainspec::EthChainSpec, optimism::node::OpDAConfig},
	reth_optimism_node::payload::config::OpGasLimitConfig,
	serde::{Deserialize, Serialize},
	std::time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default)]
pub struct OptimismDefaultLimits;

#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OpLimitsExt {
	pub max_tx_da: Option<u64>,
	pub max_block_da: Option<u64>,
	pub max_block_da_footprint: Option<u64>,
}

impl Default for OpLimitsExt {
	fn default() -> Self {
		let gas_config = OpGasLimitConfig::default();
		let da_config = OpDAConfig::default();
		OpLimitsExt {
			max_tx_da: da_config.max_da_tx_size(),
			max_block_da: da_config.max_da_block_size(),
			max_block_da_footprint: gas_config.gas_limit(),
		}
	}
}

impl LimitExtension for OpLimitsExt {
	fn clamp(&self, other: &Self) -> Self {
		Self {
			max_tx_da: self.max_tx_da.min(other.max_tx_da),
			max_block_da: self.max_block_da.min(other.max_block_da),
			max_block_da_footprint: self
				.max_block_da_footprint
				.min(other.max_block_da_footprint),
		}
	}
}

impl<P> PlatformLimits<P> for OptimismDefaultLimits
where
	P: Platform<
			ExtraLimits = OpLimitsExt,
			NodeTypes: NodeTypes<
				ChainSpec = types::ChainSpec<Optimism>,
				Payload = types::PayloadTypes<Optimism>,
			>,
		>,
{
	fn create(&self, block: &BlockContext<P>) -> Limits<P> {
		let mut limits = Limits::gas_limit(
			block
				.attributes()
				.gas_limit
				.unwrap_or_else(|| block.parent().header().gas_limit()),
		)
		.with_deadline(Duration::from_secs(
			block
				.attributes()
				.payload_attributes
				.timestamp
				.saturating_sub(
					SystemTime::now()
						.duration_since(UNIX_EPOCH)
						.unwrap_or_default()
						.as_secs(),
				),
		));

		if let Some(blob_params) = block
			.chainspec()
			.blob_params_at_timestamp(block.attributes().payload_attributes.timestamp)
		{
			limits = limits.with_blob_params(blob_params);
		}

		// Extension for optimism
		let da_config = OpDAConfig::default();
		limits = limits.with_ext(OpLimitsExt {
			// 0 means no limit
			max_tx_da: da_config.max_da_tx_size().filter(|&v| v != 0),
			max_block_da: da_config.max_da_block_size().filter(|&v| v != 0),
			max_block_da_footprint: Some(limits.gas_limit),
		});

		limits
	}
}
