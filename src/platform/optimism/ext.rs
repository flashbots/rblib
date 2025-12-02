use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		eips::Encodable2718,
		evm::{
			op_revm::L1BlockInfo,
			revm::database::{State, WrapDatabaseRef},
		},
	},
	op_alloy_flz,
	reth::{api::NodeTypes, optimism::forks::OpHardforks},
};

pub trait BlockOpExt<P: Platform> {
	/// Returns `true` if [`Ecotone`] hard fork is active at the given block
	/// timestamp.
	fn is_ecotone_active(&self) -> bool;

	/// Returns `true` if [`Holocene`] hard fork is active at the given block
	/// timestamp.
	fn is_holocene_active(&self) -> bool;

	/// Returns `true` if [`Jovian`] hard fork is active at the given block
	/// timestamp.
	fn is_jovian_active(&self) -> bool;
}

impl<P: Platform> BlockOpExt<P> for BlockContext<P>
where
	P: Platform<
		NodeTypes: NodeTypes<
			ChainSpec = types::ChainSpec<Optimism>,
			Payload = types::PayloadTypes<Optimism>,
		>,
	>,
{
	fn is_ecotone_active(&self) -> bool {
		self
			.chainspec()
			.is_ecotone_active_at_timestamp(self.timestamp())
	}

	fn is_holocene_active(&self) -> bool {
		self
			.chainspec()
			.is_holocene_active_at_timestamp(self.timestamp())
	}

	fn is_jovian_active(&self) -> bool {
		self
			.chainspec()
			.is_jovian_active_at_timestamp(self.timestamp())
	}
}

pub trait ExecutionResultOpExt<P: Platform> {
	/// Returns the data availability bytes used by this execution.
	/// (`daUsageEstimate`) Only non-deposit transactions are counted towards da
	/// bytes usage.
	/// Deposit transactions are not counted.
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
			.filter(|tx| !tx.is_deposit())
			.map(|tx| {
				op_alloy_flz::tx_estimated_size_fjord_bytes(
					tx.encoded_2718().as_slice(),
				)
			})
			.sum()
	}
}

pub trait SpanOpExt<P: Platform> {
	/// Returns the cumulative data availability bytes used by all checkpoints in
	/// this span
	fn da_bytes_used(&self) -> u64;

	/// Returns the cumulative data availability footprint used by all checkpoints
	/// in this span
	fn da_footprint(&self) -> u64;
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

	fn da_footprint(&self) -> u64 {
		self.iter().filter_map(CheckpointOpExt::da_footprint).sum()
	}
}

pub trait CheckpointOpExt<P: Platform> {
	/// Data availability bytes used by this checkpoint.
	fn da_bytes_used(&self) -> u64;

	/// Returns the cumulative data availability bytes used by all checkpoints in
	/// the history of this checkpoint, including this checkpoint itself.
	fn cumulative_da_bytes_used(&self) -> u64;

	/// Returns the data availability footprint gas scalar (L1 Block Attribute)
	fn da_footprint_gas_scalar(&self) -> Option<u16>;

	/// Returns the data availability footprint
	fn da_footprint(&self) -> Option<u64> {
		self
			.da_footprint_gas_scalar()
			.map(|scalar| self.da_bytes_used() * u64::from(scalar))
	}

	/// Returns the cumulative data availability footprint used by all checkpoints
	/// in the history of this checkpoint, including this checkpoint itself.
	fn cumulative_da_footprint(&self) -> Option<u64>;

	/// Returns the blob fields for the header.
	///
	/// After jovian: this will return `da_footprint`.
	/// After Ecotone: this will always return Some(0) (blobs aren't supported)
	/// Pre-Ecotone: these fields aren't used.
	fn blob_fields(&self) -> (Option<u64>, Option<u64>);
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

	fn da_footprint_gas_scalar(&self) -> Option<u16> {
		self.block().is_jovian_active().then(|| {
			let mut state = State::builder()
				.with_database(WrapDatabaseRef(self))
				.build();
			// fetch from storage. It could be memoized as it's set once at the start
			// of the block
			L1BlockInfo::fetch_da_footprint_gas_scalar(&mut state).ok()
		})?
	}

	fn cumulative_da_footprint(&self) -> Option<u64> {
		self
			.block()
			.is_jovian_active()
			.then(|| self.history().da_footprint())
	}

	fn blob_fields(&self) -> (Option<u64>, Option<u64>) {
		// Jovian
		if self.block().is_jovian_active() {
			let footprint = self
				.cumulative_da_footprint()
				.expect("Jovian but no da footprint");
			(Some(0), Some(footprint))
		}
		// Ecotone
		else if self.block().is_ecotone_active() {
			(Some(0), Some(0))
		}
		// Pre-Ecotone
		else {
			(None, None)
		}
	}
}
