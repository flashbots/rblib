use {
	crate::{
		alloy::consensus::BlockHeader,
		prelude::*,
		reth::{
			chainspec::EthChainSpec,
			node::builder::PayloadBuilderAttributes,
			payload::builder::EthereumBuilderConfig,
		},
	},
	core::time::Duration,
	std::time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default)]
pub struct EthereumDefaultLimits(EthereumBuilderConfig);

impl EthereumDefaultLimits {
	pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
		self.0 = self.0.with_gas_limit(gas_limit);
		self
	}
}

impl PlatformLimits<Ethereum> for EthereumDefaultLimits {
	fn create(&self, block: &BlockContext<Ethereum>) -> Limits<Ethereum> {
		let timestamp = block.attributes().timestamp();
		let parent_gas_limit = block.parent().gas_limit();
		let gas_limit = self.0.gas_limit(parent_gas_limit);
		let mut limits =
			Limits::gas_limit(gas_limit).with_deadline(Duration::from_secs(
				block.attributes().timestamp().saturating_sub(
					SystemTime::now()
						.duration_since(UNIX_EPOCH)
						.unwrap_or_default()
						.as_secs(),
				),
			));

		if let Some(blob_params) =
			block.chainspec().blob_params_at_timestamp(timestamp)
		{
			limits = limits.with_blob_params(blob_params);
		}

		limits
	}
}
