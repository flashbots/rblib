use {
	crate::{alloy::eips::eip7840::BlobParams, prelude::*},
	core::time::Duration,
	serde::{Deserialize, Serialize},
	std::{fmt::Debug, time::Instant},
};

/// This type specifies the limits that payloads should stay within.
///
/// Notes:
/// - `gas_limit` is a required field as it is fundamental to block building.
/// - Limits are specified on a pipeline scope level. If a pipeline scope
///   doesn't explicitly specify its limits it will inherit its enclosing scope
///   limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits<P: Platform> {
	/// The maximum cumulative gas that can be used in the block.
	/// This includes all transactions, epilogues, prologues, and other
	/// gas-consuming operations.
	pub gas_limit: u64,

	/// Limits for blob transactions in the block.
	pub blob_params: Option<BlobParams>,

	/// The maximum number of transactions that can be included in the block.
	///
	/// This is not a standard known ethereum limit; however, it can be imposed
	/// by custom limits factories.
	pub max_transactions: Option<usize>,

	/// The time a pipeline is allowed to spend on execution.
	pub deadline: Option<Duration>,

	/// Per platform extension.
	pub ext: P::ExtraLimits,
}

impl<P: Platform> Copy for Limits<P> {}

/// This trait must be implemented by extension limits
pub trait LimitExtension: Default + Copy + Debug + Send + Sync {
	#[must_use]
	fn clamp(&self, other: &Self) -> Self;
}

impl LimitExtension for () {
	fn clamp(&self, (): &Self) -> Self {}
}

/// Types implementing this trait are responsible for calculating top-level
/// payload limits for payload jobs. This trait is usually part of the platform
/// definition.
///
/// Implementations of this type are always called exactly once at the beginning
/// of a new payload job.
pub trait PlatformLimits<P: Platform>: Default + Send + Sync + 'static {
	fn create(&self, block: &BlockContext<P>) -> Limits<P>;
}

impl<P: Platform> Limits<P> {
	pub fn gas_limit(gas_limit: u64) -> Self {
		Self {
			gas_limit,
			blob_params: None,
			deadline: None,
			max_transactions: None,
			ext: P::ExtraLimits::default(),
		}
	}

	#[must_use]
	pub fn with_blob_params(mut self, blob_params: BlobParams) -> Self {
		self.blob_params = Some(blob_params);
		self
	}

	#[must_use]
	pub fn with_max_transactions(mut self, max_transactions: usize) -> Self {
		self.max_transactions = Some(max_transactions);
		self
	}

	#[must_use]
	pub fn with_deadline_at(mut self, deadline: Instant) -> Self {
		self.deadline = Some(deadline.duration_since(Instant::now()));
		self
	}

	#[must_use]
	pub fn with_deadline(mut self, deadline: Duration) -> Self {
		self.deadline = Some(deadline);
		self
	}

	#[must_use]
	pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
		self.gas_limit = gas_limit;
		self
	}

	#[must_use]
	pub fn with_ext(mut self, ext: P::ExtraLimits) -> Self {
		self.ext = ext;
		self
	}

	#[must_use]
	pub fn clamp(self, other: &Self) -> Self {
		Self {
			gas_limit: self.gas_limit.min(other.gas_limit),
			blob_params: match (self.blob_params, other.blob_params) {
				(Some(self_params), Some(other_params)) => Some(BlobParams {
					target_blob_count: self_params
						.target_blob_count
						.min(other_params.target_blob_count),
					max_blob_count: self_params
						.max_blob_count
						.min(other_params.max_blob_count),
					update_fraction: self_params
						.update_fraction
						.min(other_params.update_fraction),
					min_blob_fee: self_params.min_blob_fee.min(other_params.min_blob_fee),
					max_blobs_per_tx: self_params
						.max_blobs_per_tx
						.min(other_params.max_blobs_per_tx),
					blob_base_cost: self_params
						.blob_base_cost
						.min(other_params.blob_base_cost),
				}),
				_ => self.blob_params.or(other.blob_params),
			},
			max_transactions: self.max_transactions.min(other.max_transactions),
			deadline: self.deadline.min(other.deadline),
			ext: self.ext.clamp(&other.ext),
		}
	}
}
