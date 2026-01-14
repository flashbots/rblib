//! Pipeline execution
//!
//! A pipeline is a declarative description of the steps that need to be
//! executed to arrive at a desired payload or signal the inability to do so.
//!
//! The pipeline definition itself does not have any logic for executing
//! steps, enforcing limits or nested pipelines. This is done by the execution
//! engine.
//!
//! Before running the pipeline, the payload will have the platform-specific
//! [`BlockBuilder::apply_pre_execution_changes`] applied to its state.
//!
//! [`BlockBuilder::apply_pre_execution_changes`]: crate::reth::evm::execute::BlockBuilder::apply_pre_execution_changes

use {
	super::{StepInstance, metrics},
	crate::{prelude::*, reth},
	core::pin::Pin,
	futures::FutureExt,
	navi::{StepNavigator, StepPath},
	reth::payload::builder::{PayloadBuilderError, PayloadId},
	scope::RootScope,
	std::sync::Arc,
	tracing::{debug, trace},
};

pub(super) mod navi;
pub(super) mod scope;

pub(super) type PipelineOutput<P> =
	Result<types::BuiltPayload<P>, Arc<PayloadBuilderError>>;

pub(super) type PipelineFuture<P> =
	Pin<Box<dyn Future<Output = PipelineOutput<P>> + Send>>;

/// This type is responsible for executing a single run of a pipeline.
///
/// The execution is driven by an async function. Call `into_future()` to get
/// the future that executes the pipeline.
pub(super) struct PipelineExecutor<P>
where
	P: Platform,
{
	payload_id: PayloadId,
	future: PipelineFuture<P>,
}

impl<P: Platform> PipelineExecutor<P> {
	pub(super) fn run(
		pipeline: Arc<Pipeline<P>>,
		block: BlockContext<P>,
		metrics: Arc<metrics::Payload>,
	) -> Self {
		pipeline.events.publish(PayloadJobStarted(block.clone()));
		metrics.jobs_started.increment(1);
		metrics.record_payload_job_attributes::<P>(block.attributes());

		let payload_id = block.payload_id();
		let future = Self::execute(pipeline, block, metrics).boxed();

		Self { payload_id, future }
	}

	pub(super) fn payload_id(&self) -> PayloadId {
		self.payload_id
	}

	pub(super) fn into_future(self) -> PipelineFuture<P> {
		self.future
	}

	async fn execute(
		pipeline: Arc<Pipeline<P>>,
		block: BlockContext<P>,
		metrics: Arc<metrics::Payload>,
	) -> PipelineOutput<P> {
		let checkpoint = block.start();
		let scope = Arc::new(RootScope::new(&pipeline, &checkpoint));

		let init_result =
			Self::initialize(&pipeline, &block, &scope, checkpoint).await;

		let output = match init_result {
			Ok(checkpoint) => {
				Self::run_steps(&pipeline, &block, &scope, checkpoint).await
			}
			Err(e) => Err(Arc::new(e)),
		};

		Self::finalize(&pipeline, &block, &metrics, &scope, output).await
	}

	async fn initialize(
		pipeline: &Arc<Pipeline<P>>,
		block: &BlockContext<P>,
		scope: &Arc<RootScope<P>>,
		checkpoint: Checkpoint<P>,
	) -> Result<Checkpoint<P>, PayloadBuilderError> {
		for step in pipeline.iter_steps() {
			let navi = step
				.navigator(pipeline)
				.expect("Invalid step path in pipeline executor");
			let limits = scope.limits_of(&step).expect("invalid step path");
			let ctx = StepContext::new(block, &navi, limits, None);
			navi.instance().before_job(ctx).await?;
		}

		trace!("{pipeline} initialized successfully");
		Ok(checkpoint)
	}

	async fn run_steps(
		pipeline: &Arc<Pipeline<P>>,
		block: &BlockContext<P>,
		scope: &Arc<RootScope<P>>,
		mut checkpoint: Checkpoint<P>,
	) -> PipelineOutput<P> {
		let Some(mut navigator) = StepNavigator::entrypoint(pipeline) else {
			debug!(
				"empty pipeline, building empty payload for attributes: {:?}",
				block.attributes()
			);
			return block.start().build_payload().map_err(Arc::new);
		};

		scope.enter(&checkpoint);

		loop {
			let path: StepPath = navigator.path().clone();
			scope.switch_context(&path, &checkpoint);

			let limits = scope.limits_of(&path).expect("invalid step path");
			let entered_at = scope.entered_at(&path);
			let ctx = StepContext::new(block, &navigator, limits, entered_at);
			let step = Arc::clone(navigator.instance());

			trace!("{pipeline} will execute step {path}");
			let output = step.step(checkpoint, ctx).await;
			trace!("{pipeline} step {path:?} completed with output: {output:#?}");

			let (next_navigator, next_checkpoint) = match output {
				ControlFlow::Ok(cp) => (navigator.next_ok(), cp),
				ControlFlow::Break(cp) => (navigator.next_break(), cp),
				ControlFlow::Fail(e) => return Err(Arc::new(e)),
			};

			checkpoint = next_checkpoint;

			match next_navigator {
				Some(next) => {
					navigator = next;
					// Cooperative yielding: The `run_steps()` loop can execute multiple
					// pipeline steps in a tight sequence without yielding back to
					// the tokio scheduler, especially when steps complete quickly
					// (synchronously). This can cause executor starvation where other
					// tasks are delayed.
					//
					// The `yield_now().await` here provides a yield point to prevent
					// starvation. This yield point is not at a fixed location - it
					// could be placed at several alternative points in the execution
					// loop. For example:
					//   - After step execution completes
					//   - Before executing the next step
					//   - At the end of each loop iteration
					//
					// The critical requirement is that SOME yield point exists in the
					// loop to allow the tokio scheduler to run other tasks. The
					// exact placement is a trade-off that hasn't been fully explored:
					//   - Here: Minimal overhead (one yield per step), but may yield
					//     unnecessarily
					//   - Before step: Never misses a yield, but always adds overhead
					//   - Conditional (e.g., "if step completed synchronously"): Optimal,
					//     but requires additional bookkeeping/changes to the step API
					//
					// If executor starvation persists, consider:
					//   1. Adding yield points to `initialize()` and `finalize()` loops
					//      (which lack await points between iterations)
					//   2. Moving yield point to different location in this loop
					tokio::task::yield_now().await;
				}
				None => return checkpoint.build_payload().map_err(Arc::new),
			}
		}
	}

	async fn finalize(
		pipeline: &Arc<Pipeline<P>>,
		block: &BlockContext<P>,
		metrics: &Arc<metrics::Payload>,
		scope: &Arc<RootScope<P>>,
		output: PipelineOutput<P>,
	) -> PipelineOutput<P> {
		let output = Arc::new(output.map_err(|e| clone_payload_error_lossy(&e)));

		for step in pipeline.iter_steps() {
			let navi = step
				.navigator(pipeline)
				.expect("Invalid step path in pipeline executor");
			let limits = scope.limits_of(&step).expect("invalid step path");
			let step_ctx = StepContext::new(block, &navi, limits, None);

			if let Err(e) = navi.instance().after_job(step_ctx, output.clone()).await
			{
				trace!("{pipeline} finalization failed with error: {e:?}");
				return Err(Arc::new(e));
			}
		}

		if scope.is_active() {
			scope.leave();
		}

		let result = Arc::into_inner(output)
			.expect("unexpected > 1 strong reference count")
			.map_err(Arc::new);

		trace!("{pipeline} completed with output: {result:#?}");

		let payload_id = block.payload_id();
		let events_bus = &pipeline.events;

		match &result {
			Ok(built_payload) => {
				events_bus.publish(PayloadJobCompleted::<P> {
					payload_id,
					built_payload: built_payload.clone(),
				});
				metrics.jobs_completed.increment(1);
				metrics.record_payload::<P>(built_payload, block);
			}
			Err(error) => {
				events_bus.publish(PayloadJobFailed {
					payload_id,
					error: error.clone(),
				});
				metrics.jobs_failed.increment(1);
			}
		}

		result
	}
}

pub(crate) fn clone_payload_error_lossy(
	error: &PayloadBuilderError,
) -> PayloadBuilderError {
	match error {
		PayloadBuilderError::MissingParentHeader(fixed_bytes) => {
			PayloadBuilderError::MissingParentHeader(*fixed_bytes)
		}
		PayloadBuilderError::MissingParentBlock(fixed_bytes) => {
			PayloadBuilderError::MissingParentBlock(*fixed_bytes)
		}
		PayloadBuilderError::ChannelClosed => PayloadBuilderError::ChannelClosed,
		PayloadBuilderError::MissingPayload => PayloadBuilderError::MissingPayload,
		PayloadBuilderError::Internal(reth_error) => {
			PayloadBuilderError::Other(reth_error.to_string().into())
		}

		PayloadBuilderError::EvmExecutionError(error)
		| PayloadBuilderError::Other(error) => {
			PayloadBuilderError::Other(error.to_string().into())
		}
	}
}
