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

use {
	super::{StepInstance, service::ServiceContext},
	crate::{prelude::*, reth},
	core::{
		pin::Pin,
		task::{Context, Poll},
	},
	futures::FutureExt,
	navi::{StepNavigator, StepPath},
	reth::payload::builder::{PayloadBuilderError, PayloadId},
	scope::RootScope,
	std::{sync::Arc, time::Instant},
	tracing::{debug, trace},
};

pub(super) mod navi;
pub(super) mod scope;

type PipelineOutput<P: Platform> =
	Result<types::BuiltPayload<P>, Arc<PayloadBuilderError>>;

/// This type is responsible for executing a single run of a pipeline.
///
/// The execution is driven by the future poll that it implements. Each call to
/// `poll` will run one step of the pipeline at a time, or parts of a step if
/// the step is async and needs many polls before it completes. The executor
/// future will resolve when the whole pipeline has been executed, or when an
/// error occurs.
pub(super) struct PipelineExecutor<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	/// The current state of the executor state machine.
	cursor: Cursor<P>,

	// The pipeline that is being executed.
	pipeline: Arc<Pipeline<P>>,

	// The block context that is being built.
	block: BlockContext<P>,

	// The reth payload builder service context that is running this payload job.
	service: Arc<ServiceContext<P, Provider>>,

	/// Execution scopes. This root scope represents the top-level pipeline that
	/// may contain nested scopes for each nested pipeline.
	scope: Arc<RootScope<P>>,
}

impl<P: Platform, Provider: traits::ProviderBounds<P>>
	PipelineExecutor<P, Provider>
{
	/// Begins the execution of a pipeline for a new block/payload job.
	pub(super) fn run(
		pipeline: Arc<Pipeline<P>>,
		block: BlockContext<P>,
		service: Arc<ServiceContext<P, Provider>>,
	) -> Self {
		// Emit a system event for this new payload job and record initial metrics.
		pipeline.events.publish(PayloadJobStarted(block.clone()));
		service.metrics().jobs_started.increment(1);
		service
			.metrics()
			.record_payload_job_attributes::<P>(block.attributes());

		// Create the initial payload checkpoint, this will implicitly capture the
		// time we started executing the pipeline for this payload job.
		let checkpoint = block.start();

		// initialize pipeline scopes
		let root = Arc::new(RootScope::new(&pipeline, &checkpoint));

		// Initially set the execution cursor to initializing state, that will call
		// all `before_job` methods of the steps in the pipeline.
		Self {
			cursor: Cursor::<P>::Initializing({
				let block = block.clone();
				let pipeline = Arc::clone(&pipeline);
				let scope = Arc::clone(&root);

				async move {
					for step in pipeline.iter_steps() {
						let navi = step.navigator(&pipeline).expect(
							"Invalid step path. This is a bug in the pipeline executor \
							 implementation.",
						);
						let limits = scope.limits_of(&step).expect("invalid step path");
						let ctx = StepContext::new(&block, &navi, limits, None);
						navi.instance().before_job(ctx).await?;
					}

					Ok(checkpoint)
				}
				.boxed()
			}),
			pipeline,
			block,
			service,
			scope: root,
		}
	}

	/// Returns the payload id for which we are building a payload.
	pub(super) fn payload_id(&self) -> PayloadId {
		self.block.payload_id()
	}
}

/// private implementation details for the `PipelineExecutor`.
impl<P: Platform, Provider: traits::ProviderBounds<P>>
	PipelineExecutor<P, Provider>
{
	/// This method creates a future that encapsulates the execution as an async
	/// step. The created future will be held inside `Cursor::StepInProgress` and
	/// polled until it resolves.
	///
	/// It will prepare the step context and all the information a pipeline step
	/// needs to execute, then create a future object that will be stored in the
	/// cursor and polled whenever the executor is polled.
	fn execute_step(
		&self,
		path: &StepPath,
		input: Checkpoint<P>,
	) -> Pin<Box<dyn Future<Output = ControlFlow<P>> + Send>> {
		let limits = self.scope.limits_of(path).expect(
			"Invalid step path. This is a bug in the pipeline executor \
			 implementation.",
		);

		let navi = path.navigator(&self.pipeline).expect(
			"Invalid step path. This is a bug in the pipeline executor \
			 implementation.",
		);

		let entered_at = self.scope.entered_at(path);
		let ctx = StepContext::new(&self.block, &navi, limits, entered_at);
		let step = Arc::clone(navi.instance());
		async move { step.step(input, ctx).await }.boxed()
	}

	/// This method handles the control flow of the pipeline execution.
	///
	/// Once a step is executed it determines the next step to execute based on
	/// the output of the step, the current cursor state and the pipeline
	/// structure.
	fn advance_cursor(
		&self,
		current_path: &StepPath,
		output: ControlFlow<P>,
	) -> Cursor<P> {
		// we need this type to determine the next step to execute
		// based on the current step output.
		let navigator = current_path.navigator(&self.pipeline).expect(
			"Step path is unreachable. This is a bug in the pipeline executor \
			 implementation.",
		);

		// identify the next step to execute based on the output of the previously
		// executed step.
		let (step, input) = match output {
			// If the step output is a failure, we terminate the execution of the
			// whole pipeline and return the error as the final output on next future
			// poll.
			ControlFlow::Fail(payload_builder_error) => {
				return Cursor::Finalizing(
					self.finalize(Err(Arc::new(payload_builder_error))),
				);
			}

			// not a failure, chose the next step based on the control flow output
			ControlFlow::Break(checkpoint) => (navigator.next_break(), checkpoint),
			ControlFlow::Ok(checkpoint) => (navigator.next_ok(), checkpoint),
		};

		let Some(step) = step else {
			// If there is no next step, we are done with the pipeline execution.
			// We can finalize the pipeline and return the output as the final
			// result of the pipeline run.
			return Cursor::Finalizing(
				self.finalize(input.build_payload().map_err(Arc::new)),
			);
		};

		// there is a next step to be executed, create a cursor that will
		// start running the next step with the output of the current step
		// as input on next executor future poll

		// enter the scope of the next step
		self.scope.switch_context(step.path(), &input);

		// schedule execution on next future poll
		Cursor::BeforeStep(step.into(), input)
	}

	/// After pipeline steps are initialized, this method will identify the first
	/// step to execute in the pipeline and prepare the cursor to run it.
	fn first_step(&self, checkpoint: Checkpoint<P>) -> Cursor<P> {
		let Some(navigator) = StepNavigator::entrypoint(&self.pipeline) else {
			debug!(
				"empty pipeline, building empty payload for attributes: {:?}",
				self.block.attributes()
			);

			return Cursor::<P>::Finalizing(
				self.finalize(self.block.start().build_payload().map_err(Arc::new)),
			);
		};

		// enter the scope of the root pipeline
		self.scope.enter(&checkpoint);

		// Begin executing the first step of the pipeline in the next future poll
		Cursor::BeforeStep(navigator.into(), checkpoint)
	}

	/// This method will walk through the pipeline steps and invoke the
	/// `after_job` method of each step in the pipeline with the final output.
	fn finalize(
		&self,
		output: PipelineOutput<P>,
	) -> Pin<Box<dyn Future<Output = PipelineOutput<P>> + Send>> {
		let output = Arc::new(output.map_err(|e| clone_payload_error_lossy(&e)));
		let pipeline = Arc::clone(&self.pipeline);
		let block = self.block.clone();
		let pipeline = Arc::clone(&pipeline);
		let scope = Arc::clone(&self.scope);

		async move {
			// invoke the `after_job` method of each step in the pipeline
			// if any of them fails, we fail the pipeline execution, otherwise
			// we return the output of the pipeline.
			for step in pipeline.iter_steps() {
				let navi = step.navigator(&pipeline).expect(
					"Invalid step path. This is a bug in the pipeline executor \
					 implementation.",
				);
				let limits = scope.limits_of(&step).expect("invalid step path");
				let ctx = StepContext::new(&block, &navi, limits, None);
				navi.instance().after_job(ctx, output.clone()).await?;
			}

			// leave the scope of the root pipeline (if entered, we never enter the
			// root scope only in empty pipelines).
			if scope.is_active() {
				scope.leave();
			}

			Arc::into_inner(output)
				.expect("unexpected > 1 strong reference count")
				.map_err(Arc::new)
		}
		.boxed()
	}
}

impl<P, Provider> Future for PipelineExecutor<P, Provider>
where
	P: Platform,
	Provider: traits::ProviderBounds<P>,
{
	type Output = PipelineOutput<P>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let executor = self.get_mut();

		// The executor has not run any steps yet, it is invoking the `before_job`
		// method of each step in the pipeline.
		if let Cursor::Initializing(ref mut future) = executor.cursor {
			if let Poll::Ready(output) = future.as_mut().poll_unpin(cx) {
				match output {
					Ok(checkpoint) => {
						trace!("{} initialized successfully", executor.pipeline);
						executor.cursor = executor.first_step(checkpoint);
					}
					Err(error) => {
						trace!(
							"{} initialization failed with error: {error:?}",
							executor.pipeline
						);
						// If the initialization failed, we immediately finalize the
						// pipeline with the error that occurred during initialization
						// and not attempt to run any steps.
						executor.cursor =
							Cursor::Finalizing(executor.finalize(Err(error.into())));
					}
				}
				trace!("{} initializing completed", executor.pipeline);
			}

			// tell the async runtime to poll again because we are still initializing
			cx.waker().wake_by_ref();
		}

		// the pipeline has completed executing all steps or encountered and error.
		// Now we are running the `after_job` of each step in the pipeline.
		if let Cursor::Finalizing(ref mut future) = executor.cursor {
			if let Poll::Ready(output) = future.as_mut().poll_unpin(cx) {
				trace!("{} completed with output: {output:#?}", executor.pipeline);

				// Execution of this pipeline has completed, This resolves the
				// executor future with the final output of the pipeline. Also
				// emit an appropriate system event and record metrics.

				let payload_id = executor.block.payload_id();
				let events_bus = &executor.pipeline.events;
				let metrics = executor.service.metrics();

				match &output {
					Ok(built_payload) => {
						events_bus.publish(PayloadJobCompleted::<P> {
							payload_id,
							built_payload: built_payload.clone(),
						});
						metrics.jobs_completed.increment(1);
						metrics.record_payload::<P>(built_payload, &executor.block);
					}
					Err(error) => {
						events_bus.publish(PayloadJobFailed {
							payload_id,
							error: error.clone(),
						});
						metrics.jobs_failed.increment(1);
					}
				}

				return Poll::Ready(output);
			}

			// tell the async runtime to poll again because we are still finalizing
			cx.waker().wake_by_ref();
		}

		if matches!(executor.cursor, Cursor::BeforeStep(_, _)) {
			// If the cursor is in the BeforeStep state, we need to run the next
			// step of the pipeline. Steps are async futures, so we need to store
			// their instance while they are running and being polled until resolved.

			let Cursor::BeforeStep(path, input) =
				std::mem::replace(&mut executor.cursor, Cursor::PreparingStep)
			else {
				unreachable!("bug in PipelineExecutor state machine");
			};

			trace!("{} will execute step {path}", executor.pipeline);
			let future = executor.execute_step(&path, input);
			executor.cursor = Cursor::StepInProgress(path, future);
			cx.waker().wake_by_ref(); // tell the async runtime to poll again
		}

		if let Cursor::StepInProgress(ref path, ref mut future) = executor.cursor {
			// If the cursor is in the StepInProgress state, we to poll the
			// future instance of that step to see if it has completed.
			if let Poll::Ready(output) = future.as_mut().poll_unpin(cx) {
				trace!(
					"{} step {path:?} completed with output: {output:#?}",
					executor.pipeline
				);

				// step has completed, we can advance the cursor
				executor.cursor = executor.advance_cursor(path, output);
			}

			cx.waker().wake_by_ref(); // tell the async runtime to poll again
		}

		Poll::Pending
	}
}

/// Keeps track of the current pipeline execution progress.
enum Cursor<P: Platform> {
	/// The pipeline will execute this step on the next iteration.
	BeforeStep(StepPath, Checkpoint<P>),

	/// a pipeline step execution is in progress.
	///
	/// This state is set when the pipeline executor has began executing a step
	/// but has not yet completed it between two polls. Steps are asynchronous
	/// and may span multiple polls. Until the step future is resolved, we need
	/// to store its instance and poll it.
	///
	/// Here we store the step path that is currently being executed
	/// and the pinned future that is executing the step.
	StepInProgress(
		StepPath,
		Pin<Box<dyn Future<Output = ControlFlow<P>> + Send>>,
	),

	/// The pipeline is currently initializing all steps for a new payload job.
	///
	/// This happens once before any step is executed, and it calls the
	/// `before_job` method of each step in the pipeline.
	Initializing(
		Pin<
			Box<
				dyn Future<Output = Result<Checkpoint<P>, PayloadBuilderError>> + Send,
			>,
		>,
	),

	/// This state occurs after the `Completed` state is reached. It calls
	/// the `after_job` method of each step in the pipeline with the output.
	Finalizing(Pin<Box<dyn Future<Output = PipelineOutput<P>> + Send>>),

	/// The pipeline is currently preparing to execute the next step.
	/// We are in this state only for a brief moment inside the `poll` method, and
	/// it will never be seen by the `run_step` method.
	PreparingStep,
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
