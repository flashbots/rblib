use {
	crate::{alloy, prelude::*, reth},
	alloy::eips::Encodable2718,
	op_alloy_flz,
	reth::api::NodeTypes,
};

pub trait ExecutionResultOpExt<P: Platform> {
	/// Data availability bytes used by this execution.
	fn da_bytes_used(&self) -> u64;
}

impl<P: Platform> ExecutionResultOpExt<P> for ExecutionResult<P>
where
	P: Platform<
		NodeTypes: NodeTypes<
			ChainSpec = types::ChainSpec<Optimism>,
			Primitives = types::Primitives<Optimism>,
			Payload = types::PayloadTypes<Optimism>,
		>,
	>,
{
	fn da_bytes_used(&self) -> u64 {
		self
			.source
			.transactions()
			.iter()
			.map(|tx| {
				op_alloy_flz::tx_estimated_size_fjord_bytes(
					tx.encoded_2718().as_slice(),
				)
			})
			.sum()
	}
}

pub trait SpanOpExt<P: Platform> {
	/// Returns the total data availability bytes used by all checkpoints in this
	/// span
	fn da_bytes_used(&self) -> u64;
}

impl<P: Platform> SpanOpExt<P> for Span<P>
where
	P: Platform<
		NodeTypes: NodeTypes<
			ChainSpec = types::ChainSpec<Optimism>,
			Primitives = types::Primitives<Optimism>,
			Payload = types::PayloadTypes<Optimism>,
		>,
	>,
{
	fn da_bytes_used(&self) -> u64 {
		self.iter().map(CheckpointOpExt::da_bytes_used).sum()
	}
}

pub trait CheckpointOpExt<P: Platform> {
	/// Data availability bytes used by this checkpoint.
	fn da_bytes_used(&self) -> u64;

	/// Returns the cumulative data availability bytes used by all checkpoints in
	/// the history of this checkpoint, including this checkpoint itself.
	fn cumulative_da_bytes_used(&self) -> u64;
}

impl<P: Platform> CheckpointOpExt<P> for Checkpoint<P>
where
	P: Platform<
		NodeTypes: NodeTypes<
			ChainSpec = types::ChainSpec<Optimism>,
			Primitives = types::Primitives<Optimism>,
			Payload = types::PayloadTypes<Optimism>,
		>,
	>,
{
	fn da_bytes_used(&self) -> u64 {
		self.result().map_or(0, |result| result.da_bytes_used())
	}

	fn cumulative_da_bytes_used(&self) -> u64 {
		self.history().da_bytes_used()
	}
}
