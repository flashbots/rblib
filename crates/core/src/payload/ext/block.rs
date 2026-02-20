use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::BlockHeader,
		primitives::{Address, U256},
	},
	reth::{
		errors::ProviderResult,
		node::builder::PayloadBuilderAttributes,
		payload::builder::PayloadId,
	},
};

/// Quality of Life extensions for the `BlockContext` type.
pub trait BlockExt<P: Platform>: super::sealed::Sealed {
	/// Returns the payload ID that was supplied by the CL client
	/// during the `ForkchoiceUpdated` request inside the payload attributes.
	fn payload_id(&self) -> PayloadId;

	/// Returns the timestamp of the block for which the payload is being built.
	fn timestamp(&self) -> u64;

	/// Returns the number of the block for which the payload is being built.
	fn number(&self) -> u64;

	/// Returns the base fee for the block under construction.
	fn base_fee(&self) -> u64;

	/// Returns the blob fee for the block under construction, if applicable.
	fn blob_fee(&self) -> Option<u128>;

	/// Returns the maximum blob gas allowed for this block.
	fn blob_gas_limit(&self) -> u64;

	/// Address of the fees recipient for the block.
	fn coinbase(&self) -> Address;

	/// Returns the balance of the given address at the beginning of the block
	/// before any transactions are executed.
	fn balance_of(&self, address: Address) -> ProviderResult<U256>;
}

impl<P: Platform> BlockExt<P> for BlockContext<P> {
	/// Returns the payload ID that was supplied by the CL client
	/// during the `ForkchoiceUpdated` request inside the payload attributes.
	fn payload_id(&self) -> PayloadId {
		self.attributes().payload_id()
	}

	/// Returns the timestamp of the block for which the payload is being built.
	fn timestamp(&self) -> u64 {
		self.attributes().timestamp()
	}

	/// Returns the number of the block for which the payload is being built.
	fn number(&self) -> u64 {
		self.parent().header().number() + 1
	}

	/// Returns the base fee for the block under construction.
	fn base_fee(&self) -> u64 {
		self
			.parent()
			.header()
			.next_block_base_fee(
				reth::chainspec::EthChainSpec::base_fee_params_at_timestamp(
					&self.chainspec(),
					self.attributes().timestamp(),
				),
			)
			.unwrap_or_default()
	}

	/// Returns the blob fee for the block under construction, if applicable.
	fn blob_fee(&self) -> Option<u128> {
		reth::chainspec::EthChainSpec::blob_params_at_timestamp(
			&self.chainspec(),
			self.attributes().timestamp(),
		)
		.map(|blob_params| {
			self
				.parent()
				.header()
				.blob_fee(blob_params)
				.unwrap_or_default()
		})
	}

	/// Returns the maximum blob gas allowed for this block.
	fn blob_gas_limit(&self) -> u64 {
		reth::chainspec::EthChainSpec::blob_params_at_timestamp(
			&self.chainspec(),
			self.attributes().timestamp(),
		)
		.map(|params| params.max_blob_gas_per_block())
		.unwrap_or_default()
	}

	/// Address of the fees recipient for the block.
	fn coinbase(&self) -> Address {
		self.attributes().suggested_fee_recipient()
	}

	/// Returns the balance of the given address at the beginning of the block
	/// before any transactions are executed.
	fn balance_of(&self, address: Address) -> ProviderResult<U256> {
		self
			.base_state()
			.account_balance(&address)
			.map(|balance| balance.unwrap_or_default())
	}
}
