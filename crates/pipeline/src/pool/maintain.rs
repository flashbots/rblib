//! Order pool maintenance utilities.
//!
//! This is not a public API, but rather a set of utilities that are used
//! internally by the order pool to maintain its state and react to events.

use {
	super::*,
	futures::{FutureExt, Stream, StreamExt},
};

/// The most buffered events of one kind that are recorded from a pipeline
/// event stream after the pipeline drop was observed. The pipeline events bus
/// buffers 100 events per kind, so the bound is not reached in practice; it
/// only guarantees that the drain terminates.
const MAX_EVENT_DRAIN: usize = 256;

/// Applies `handle` to every event that `stream` has already buffered,
/// without waiting for more, bounded by [`MAX_EVENT_DRAIN`]. Stops at the
/// first poll that is pending or at the end of the stream.
fn drain_buffered<E>(
	stream: &mut (impl Stream<Item = E> + Unpin),
	handle: impl FnMut(E),
) {
	(0..MAX_EVENT_DRAIN)
		.map_while(|_| stream.next().now_or_never().flatten())
		.for_each(handle);
}

impl<P: Platform> OrderPool<P> {
	/// Removes orders that became permanently ineligible after the given block
	/// header became the tip of the chain. Goes through [`OrderPool::remove`]
	/// so that the lifecycle log sees the eviction.
	pub(super) fn remove_invalidated_orders(
		&self,
		header: &SealedHeader<types::Header<P>>,
	) {
		// collect first: `remove` mutates `orders`, which must not happen while
		// the map is being iterated.
		self
			.inner
			.orders
			.iter()
			.filter(|entry| match entry.value() {
				Order::Bundle(bundle) => bundle.is_permanently_ineligible(header),
				Order::Transaction(_) => false,
			})
			.map(|entry| *entry.key())
			.collect::<Vec<B256>>()
			.into_iter()
			.for_each(|order_hash| self.remove(&order_hash));
	}

	/// An order was proposed by the pool and a step attempted to include it in
	/// a payload.
	fn on_inclusion_attempt(&self, event: &OrderInclusionAttempt) {
		let OrderInclusionAttempt(order, payload_id) = event;
		tracing::trace!(
			">--> order inclusion attempt: {order} in payload job {payload_id}"
		);
		self.report_inclusion_attempt(*order, *payload_id);
	}

	/// An order proposed by the pool failed to execute in a payload.
	fn on_inclusion_failure(&self, event: &OrderInclusionFailure<P>) {
		let OrderInclusionFailure(order, err, payload_id) = event;
		tracing::trace!(
			">--> order inclusion failure: {order} in payload job {payload_id} - \
			 {err}"
		);
		self.report_execution_error(*order, err);
	}

	/// An order proposed by the pool was included in a payload.
	fn on_inclusion_success(&self, event: &OrderInclusionSuccess) {
		let OrderInclusionSuccess(order, payload_id) = event;
		tracing::trace!(
			">--> order inclusion success: {order} in payload job {payload_id}"
		);
		self.report_inclusion_success(*order, *payload_id);
	}

	/// Returns the future that listens to the events of the given pipeline and
	/// reports them to the pool until the pipeline is dropped. The event streams
	/// are subscribed before this returns, so nothing published afterwards is
	/// missed. Spawned by [`OrderPool::attach_pipeline`].
	pub(crate) fn start_pipeline_events_listener(
		&self,
		pipeline: &Pipeline<P>,
	) -> impl Future<Output = eyre::Result<()>> + 'static {
		let mut inclusion = pipeline.subscribe::<OrderInclusionAttempt>();
		let mut success = pipeline.subscribe::<OrderInclusionSuccess>();
		let mut failure = pipeline.subscribe::<OrderInclusionFailure<P>>();
		let mut dropped = pipeline.subscribe::<PipelineDropped>();

		let order_pool = self.clone();

		async move {
			loop {
				// The arms are polled in order (`biased`), not at random. The
				// pipeline drop comes first so that this task always exits, however
				// busy the other streams are. Then attempts before outcomes: a step
				// emits the inclusion attempt of an order before its failure or
				// success, so draining attempts first keeps the lifecycle stages of
				// an order in emission order when several events are ready at once.
				tokio::select! {
					biased;

					Some(PipelineDropped) = dropped.next() => {
						// Pipeline was dropped, stop this maintenance task. The drop is
						// published synchronously, right after the last step emitted its
						// events, and this arm is polled first, so those events can still
						// be buffered in the other streams: record them before exiting,
						// in emission order (attempts, then failures, then successes),
						// otherwise the last stages of the pipeline would be lost.
						drain_buffered(&mut inclusion, |event| {
							order_pool.on_inclusion_attempt(&event);
						});
						drain_buffered(&mut failure, |event| {
							order_pool.on_inclusion_failure(&event);
						});
						drain_buffered(&mut success, |event| {
							order_pool.on_inclusion_success(&event);
						});
						return Ok(());
					}
					Some(event) = inclusion.next() => {
						order_pool.on_inclusion_attempt(&event);
					}
					Some(event) = failure.next() => {
						order_pool.on_inclusion_failure(&event);
					}
					Some(event) = success.next() => {
						order_pool.on_inclusion_success(&event);
					}
				}
			}
		}
	}
}
