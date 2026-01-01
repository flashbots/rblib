use {
	super::MetricsSet,
	core::{
		sync::atomic::{AtomicU32, AtomicU64, Ordering},
		time::Duration,
	},
	metrics::{Counter, Histogram},
};

#[derive(MetricsSet)]
pub(super) struct Metrics {
	/// The total number of times this step's `step` method has been invoked.
	pub invoked_total: Counter,

	/// The total number of times this step's `step` method has been invoked per
	/// payload job.
	pub invoked_per_job: Histogram,

	/// The total number of times this step's `before_job` method has been
	/// invoked.
	pub before_job_invoked_total: Counter,

	/// The number of times `before_job` has failed.
	pub before_job_failed_total: Counter,

	/// The duration of time spent executing this step's `before_job` method.
	pub before_job_exec_duration_histogram: Histogram,

	/// The cumulative number of milliseconds spent executing this step's
	/// `before_job` method across all runs.
	pub before_job_exec_duration_total_millis: Counter,

	/// The total number of times this step's `after_job` method has been
	/// invoked.
	pub after_job_invoked_total: Counter,

	/// The number of times `after_job` has failed.
	pub after_job_failed_total: Counter,

	/// The duration of time spent executing this step's `after_job` method.
	pub after_job_exec_duration_histogram: Histogram,

	/// The cumulative number of milliseconds spent executing this step's
	/// `after_job` method across all runs.
	pub after_job_exec_duration_total_millis: Counter,

	/// The total number of times this step's `step` method has returned
	/// `ControlFlow::Ok`.
	pub ok_total: Counter,

	/// The total number of times this step's `step` method has returned
	/// `ControlFlow::Break`.
	pub break_total: Counter,

	/// The total number of times this step's `step` method has returned
	/// `ControlFlow::Fail`.
	pub fail_total: Counter,

	/// The duration of time spent executing this step's `step` method.
	pub exec_duration_histogram: Histogram,

	/// The cumulative number of milliseconds spent executing this step's `step`
	/// method across all runs.
	pub exec_duration_total_millis: Counter,

	/// The duration of time spent executing this step's `step` method per
	/// payload job.
	pub exec_duration_per_job: Histogram,
}

/// Tracks metrics aggregates per job. Those counters get reset before each new
/// payload job.
#[derive(Default, Debug)]
pub(super) struct PerJobCounters {
	/// The number of times this step was invoked during the last job.
	pub invoked: AtomicU32,

	/// The duration of time spent executing this step's `step` method during the
	/// last job in milliseconds.
	pub exec_duration_micros: AtomicU64,
}

impl PerJobCounters {
	/// Called at the end of a payload job
	pub(super) fn reset(&self) {
		self.invoked.store(0, Ordering::Relaxed);
		self.exec_duration_micros.store(0, Ordering::Relaxed);
	}

	pub(super) fn increment_exec_time(&self, duration: Duration) {
		#[expect(clippy::cast_possible_truncation)]
		self
			.exec_duration_micros
			.fetch_add(duration.as_micros() as u64, Ordering::Relaxed);
	}

	pub(super) fn increment_invocation(&self) {
		self.invoked.fetch_add(1, Ordering::Relaxed);
	}

	pub(super) fn exec_duration(&self) -> Duration {
		Duration::from_micros(self.exec_duration_micros.load(Ordering::Relaxed))
	}

	pub(super) fn invoked_count(&self) -> u32 {
		self.invoked.load(Ordering::Relaxed)
	}
}
