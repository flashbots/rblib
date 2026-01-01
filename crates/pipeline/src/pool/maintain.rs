//! Order pool maintenance utilities.
//!
//! This is not a public API, but rather a set of utilities that are used
//! internally by the order pool to maintain its state and react to events.

use {super::*, futures::StreamExt};

impl<P: Platform> OrderPool<P> {
	/// Removes orders that became permanently ineligible after the given block
	/// header became the tip of the chain.
	pub(super) fn remove_invalidated_orders(
		&self,
		header: &SealedHeader<types::Header<P>>,
	) {
		self.inner.orders.retain(|_, order| {
			if let Order::Bundle(bundle) = order {
				!bundle.is_permanently_ineligible(header)
			} else {
				true
			}
		});
	}

	pub(super) fn start_pipeline_events_listener(
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
				tokio::select! {
					Some(OrderInclusionAttempt(order, payload_id)) = inclusion.next() => {
						tracing::trace!(">--> order inclusion attempt: {order} in payload job {payload_id}");
						order_pool.report_inclusion_attempt(order, payload_id);
					}
					Some(OrderInclusionSuccess(order, payload_id)) = success.next() => {
						tracing::trace!(">--> order inclusion success: {order} in payload job {payload_id}");
					}
					Some(OrderInclusionFailure(order, err, payload_id)) = failure.next() => {
						tracing::trace!(">--> order inclusion failure: {order} in payload job {payload_id} - {err}");
						order_pool.report_execution_error(order, &err);
					}
					Some(PipelineDropped) = dropped.next() => {
						// Pipeline was dropped, stop this maintenance task
						return Ok(());
					}
				}
			}
		}
	}
}
