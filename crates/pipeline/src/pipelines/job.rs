use {
	super::traits,
	crate::{
		alloy,
		pipelines::{exec::PipelineExecutor, service::ServiceContext},
		prelude::*,
		reth,
	},
	alloy::consensus::BlockHeader,
	core::{
		pin::Pin,
		task::{Context, Poll},
	},
	futures::{FutureExt, future::Shared},
	reth::{
		api::PayloadBuilderAttributes,
		node::builder::{BlockBody, BuiltPayload},
		payload::builder::{PayloadJob as RethPayloadJobTrait, *},
	},
	std::{fmt::Write as _, sync::Arc, time::Instant},
	tokio::time::Sleep,
	tracing::{debug, warn},
};

/// This type is responsible for handling one payload generation request.
///
/// A payload generation request begins when the CL node sends us FCU request
/// with payload attributes. That will trigger
/// [`PayloadJobGenerator::new_payload_job`] which in turn will create a new
/// instance of this type.
///
/// This is a long-running job that will be polled by the CL node until it is
/// resolved. The job future must resolve within 1 second from the moment
/// [`PayloadJob::resolve_kind`] is called with [`PayloadKind::Earliest`].
///
/// This job will automatically terminate after a deadline even `resolve_kind`
/// is not called.
pub(super) struct PayloadJob<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	block: BlockContext<P>,
	fut: ExecutorFuture<P, Provider>,
	/// The deadline when this job should resolve.
	deadline: Pin<Box<Sleep>>,
}

impl<P, Provider> PayloadJob<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	pub(super) fn new(
		pipeline: &Arc<Pipeline<P>>,
		block: BlockContext<P>,
		service: &Arc<ServiceContext<P, Provider>>,
	) -> Self {
		debug!(
			"New Payload Job {} with block context: {block:#?}",
			block.payload_id()
		);

		let fut = ExecutorFuture::new(PipelineExecutor::run(
			Arc::clone(pipeline),
			block.clone(),
			Arc::clone(service),
		));

		// Job should complete within deadline (12s) even if GetPayload is never
		// called. This prevents job accumulation.
		// TODO: when FCU update avalanche is fixed we could replace
		// builder.deadline with payload.attributes.timeout
		let deadline =
			Box::pin(tokio::time::sleep(service.node_config().builder.deadline));

		Self {
			block,
			fut,
			deadline,
		}
	}
}

impl<P, Provider> RethPayloadJobTrait for PayloadJob<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	type BuiltPayload = types::BuiltPayload<P>;
	type PayloadAttributes = types::PayloadBuilderAttributes<P>;
	type ResolvePayloadFuture = ExecutorFuture<P, Provider>;

	fn best_payload(&self) -> Result<Self::BuiltPayload, PayloadBuilderError> {
		unimplemented!("PayloadJob::best_payload is not implemented");
	}

	fn payload_attributes(
		&self,
	) -> Result<Self::PayloadAttributes, PayloadBuilderError> {
		Ok(self.block.attributes().clone())
	}

	fn resolve_kind(
		&mut self,
		kind: PayloadKind,
	) -> (Self::ResolvePayloadFuture, KeepPayloadJobAlive) {
		match kind {
			// This is called when the CL node sends us a GetPayload EngineAPI
			// request. The time between the call to `resolve_kind` and the
			// creation of the payload job is the block time.
			// The future that is returned here must resolve within 1 second per eth
			// protocol.
			PayloadKind::Earliest => {
				debug!(
					"Resolving the earliest payload for job {}",
					self.block.attributes().payload_id()
				);
				(self.fut.clone(), KeepPayloadJobAlive::No)
			}
			PayloadKind::WaitForPending => {
				unimplemented!("PayloadKind::WaitForPending mode not supported");
			}
		}
	}
}

/// This future is polled for the first time by the Reth runtime when the
/// `PayloadJob` is created. Here we want to immediately start executing
/// the pipeline instead of waiting for the `resolve_kind` to be called.
impl<P, Provider> Future for PayloadJob<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	type Output = Result<(), PayloadBuilderError>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();

		// Check if deadline has been reached
		// Jobs should complete after deadline even if
		// GetPayload is never called
		if this.deadline.as_mut().poll(cx).is_ready() {
			debug!(
				"payload building job {} deadline is reached",
				this.block.payload_id(),
			);
			return Poll::Ready(Ok(()));
		}

		// When the payload job future is polled, we begin executing the pipeline
		// production future immediately as well, so that the time between the
		// creation of the job and the call to `resolve_kind` is utilized for the
		// pipeline execution.
		//
		// PayloadJob is going to be dropped anyway when the `ResolvePayloadFuture`
		// is resolved.
		//
		// At this point we don't return the result of the pipeline execution, but
		// we want to ensure that any errors that occur during the pipeline
		// execution are propagated and stop they payload job future immediately.
		if let Poll::Ready(Err(e)) = this.fut.poll_unpin(cx) {
			return Poll::Ready(Err(e));
		}

		// On happy paths or in-progress pipelines, keep the future alive. Reth
		// will drop it when a payload is resolved or deadline is reached.
		Poll::Pending
	}
}

/// This future wraps the `PipelineExecutor` and is used to poll the
/// internal executor of the pipeline. Once this future is resolved, it
/// can be polled again and will return copy of the resolved payload.
pub(super) struct ExecutorFuture<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	payload_id: PayloadId,
	started_at: Instant,
	state: ExecutorFutureState<P, Provider>,
}

/// This enum allows us to wrap the `PipelineExecutor` future
/// and cache the result of the execution. Also, it makes the executor future
/// cloneable, so that many copies of the future could be returned from
/// `resolve_kind`.
///
/// Whenever any of the copies of the future is polled, it will poll the
/// executor, if any copy resolved, all copies will also resolve with the same
/// result.
enum ExecutorFutureState<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	Ready(Result<types::BuiltPayload<P>, Arc<PayloadBuilderError>>),
	Future(Shared<PipelineExecutor<P, Provider>>),
}

impl<P, Provider> ExecutorFuture<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	pub(super) fn new(executor: PipelineExecutor<P, Provider>) -> Self {
		Self {
			started_at: Instant::now(),
			payload_id: executor.payload_id(),
			state: ExecutorFutureState::Future(executor.shared()),
		}
	}

	fn log_payload_job_result(
		&self,
		result: &Result<types::BuiltPayload<P>, Arc<PayloadBuilderError>>,
	) -> core::fmt::Result {
		match &result {
			Ok(built_payload) => {
				let built_block = built_payload.block();
				let fees = built_payload.fees();

				let mut out = String::new();
				writeln!(&mut out, "Payload job {}:", self.payload_id)?;
				writeln!(&mut out, "  Status: Success")?;
				writeln!(&mut out, "  Duration: {:?}", self.started_at.elapsed())?;
				writeln!(&mut out, "  Fees: {fees}")?;
				writeln!(&mut out, "  Built Block:")?;
				writeln!(&mut out, "    Number: {:?}", built_block.number())?;
				writeln!(&mut out, "    Hash: {:?}", built_block.hash())?;
				writeln!(&mut out, "    Parent: {:?}", built_block.parent_hash())?;
				writeln!(&mut out, "    Timestamp: {:?}", built_block.timestamp())?;
				writeln!(
					&mut out,
					"    Gas: {}/{} ({}%)",
					built_block.gas_used(),
					built_block.gas_limit(),
					built_block.gas_used() * 100 / built_block.gas_limit()
				)?;
				writeln!(
					&mut out,
					"    Transactions: {}",
					built_block.body().transactions().len()
				)?;

				debug!("{out}");
			}
			Err(error) => {
				let mut out = String::new();
				writeln!(&mut out, "Payload job {}:", self.payload_id)?;
				writeln!(&mut out, "  Status: Failed")?;
				writeln!(&mut out, "  Duration: {:?}", self.started_at.elapsed())?;
				writeln!(&mut out, "  Error: {error:#?}")?;
				warn!("{out}");
			}
		}

		Ok(())
	}
}

impl<P, Provider> Future for ExecutorFuture<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	type Output = Result<types::BuiltPayload<P>, PayloadBuilderError>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		match this.state {
			// we already have a result from previous polls, return a clone of it
			ExecutorFutureState::Ready(ref result) => Poll::Ready(
				result
					.clone()
					.map_err(|e| super::exec::clone_payload_error_lossy(&e)),
			),

			// we are still in progress. keep polling the inner executor future.
			ExecutorFutureState::Future(ref mut executor) => {
				match executor.poll_unpin(cx) {
					Poll::Ready(result) => {
						// got a result. All future polls will return the result directly
						// without polling the executor again.
						let _ = this.log_payload_job_result(&result);

						this.state = ExecutorFutureState::Ready(result.clone());
						Poll::Ready(
							result
								.clone()
								.map_err(|e| super::exec::clone_payload_error_lossy(&e)),
						)
					}
					Poll::Pending => Poll::Pending,
				}
			}
		}
		.map_err(Into::into)
	}
}

/// We want this to be cloneable because the `resolve_kind` method could
/// potentially return multiple copies of the future, and we want all of them to
/// resolve with the same result at the same time.
impl<P, Provider> Clone for ExecutorFuture<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	fn clone(&self) -> Self {
		Self {
			payload_id: self.payload_id,
			started_at: self.started_at,
			state: match &self.state {
				ExecutorFutureState::Ready(result) => {
					ExecutorFutureState::Ready(result.clone())
				}
				ExecutorFutureState::Future(future) => {
					ExecutorFutureState::Future(future.clone())
				}
			},
		}
	}
}
