use {
	crate::{alloy, prelude::*, reth},
	alloy::consensus::BlockHeader,
	core::{panic::Location, time::Duration},
	metrics::{Counter, Gauge, Histogram},
	reth::node::builder::{BuiltPayload, PayloadBuilderAttributes},
	std::time::{SystemTime, UNIX_EPOCH},
};

#[derive(MetricsSet)]
pub(super) struct Payload {
	/// Number of new payload jobs that have started.
	pub jobs_started: Counter,

	/// Number of payload jobs that have produced a block payload successfully.
	pub jobs_completed: Counter,

	/// Number of payload jobs that have started and failed to produce a block
	/// payload.
	pub jobs_failed: Counter,

	/// Number of transactions in a produced payload.
	pub tx_count_histogram: Histogram,

	/// Total number of transactions in all produced payloads.
	pub tx_count_total: Counter,

	/// Amount of gas used by transactions in a produced payload.
	pub gas_used_histogram: Histogram,

	/// Total gas used by all produced payloads.
	pub gas_used_total: Counter,

	/// The percentage of gas utilized by transactions in a produced payload.
	pub gas_utilized: Histogram,

	/// Amount of gas used by blob transactions in a produced payload.
	pub blob_gas_histogram: Histogram,

	/// Total blob gas used by all blob transactions in all produced payloads.
	pub blob_gas_total: Counter,

	/// The percentage of gas utilized by blob transactions in a produced
	/// payload.
	pub blob_gas_utilized: Histogram,

	/// Fees paid by transactions in a produced payload.
	pub fees_histogram: Histogram,

	/// Total fees accumulated by all produced payloads.
	pub fees_total: Counter,

	/// The time given by the EL for the payload job to complete.
	/// This can be also interpreted as the block time.
	pub job_deadline: Gauge,

	/// The latest block number produced by the payload builder.
	/// This is not a `Gauge` because `Gauge` does not support `u64`.
	pub block_number: Counter,
}

impl Payload {
	pub(super) fn record_payload<P: Platform>(
		&self,
		payload: &types::BuiltPayload<P>,
		block: &BlockContext<P>,
	) {
		let fees: f64 = payload.fees().into();

		self.fees_histogram.record(fees);
		self.fees_total.increment(payload.fees().to::<u64>());

		let gas_used = payload.block().gas_used() as f64;
		let gas_limit = payload.block().gas_limit() as f64;
		let gas_utilization = gas_used / gas_limit;

		self.gas_used_histogram.record(gas_used);
		self.gas_used_total.increment(payload.block().gas_used());
		self.gas_utilized.record(gas_utilization);

		let blob_gas_used = payload.block().blob_gas_used().unwrap_or_default();
		let blob_gas_limit = block.blob_gas_limit();
		let blob_gas_utilization = blob_gas_used as f64 / blob_gas_limit as f64;

		self.blob_gas_histogram.record(blob_gas_used as f64);
		self.blob_gas_total.increment(blob_gas_used);
		self.blob_gas_utilized.record(blob_gas_utilization);

		self
			.tx_count_histogram
			.record(payload.block().transaction_count() as f64);
		self
			.tx_count_total
			.increment(payload.block().transaction_count() as u64);

		self.block_number.absolute(payload.block().number());
	}

	pub(super) fn record_payload_job_attributes<P: Platform>(
		&self,
		attributes: &types::PayloadBuilderAttributes<P>,
	) {
		let job_deadline = Duration::from_secs(
			attributes.timestamp().saturating_sub(
				SystemTime::now()
					.duration_since(UNIX_EPOCH)
					.unwrap_or_default()
					.as_secs(),
			),
		);
		self.job_deadline.set(job_deadline);
	}
}

/// Automatically generates a name for a pipeline based on the location where it
/// is instantiated. This is used to provide unique names for pipelines and
/// automatically generate metrics for them.
pub(super) fn auto_pipeline_name(caller: &Location) -> String {
	let file = std::path::Path::new(caller.file())
		.file_name()
		.unwrap()
		.to_str()
		.unwrap();
	let file = file.trim_end_matches(".rs");
	format!("{}_{}", file, caller.line())
}
