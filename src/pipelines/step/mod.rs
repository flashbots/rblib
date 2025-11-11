use {
	crate::{prelude::*, reth},
	core::{fmt::Debug, future::Future, marker::PhantomData},
	reth::providers::StateProviderFactory,
	std::sync::Arc,
};

mod context;
mod instance;
mod metrics;
mod name;

pub(crate) use instance::StepInstance;
pub use {context::StepContext, reth::payload::builder::PayloadBuilderError};

/// This trait defines a step in a pipeline.
///
/// Users of the SDK should implement this trait on their own types to make them
/// usable in a pipeline. A step is a unit of work that can be executed
/// independently and can be composed with other steps in a pipeline.
///
/// Steps receive a `Checkpoint` as an input, which is the state of the payload
/// that has been built so far. The step can then modify this payload, add new
/// transactions, or perform any other operations that are needed to build the
/// final payload. The output of a step is a `ControlFlow` enum that carries a
/// new version of the payload or an error that will terminate the pipeline.
///
/// The `Step` trait is generic over the `Platform` type, which allows it to
/// be used with different platforms (e.g., Ethereum, Optimism, etc.). Steps
/// can be generic over the platform they run on or specialized for a specific
/// platform.
///
/// The instance of the step is long-lived, and its lifetime is equal to the
/// lifetime of the pipeline it is part of. All invocations of the step will
/// repeatedly call into the `step` async function on the same instance.
///
/// There may be multiple instances of the same step in a pipeline.
///
/// A step instance is guaranteed to not be called concurrently by the runtime.
pub trait Step<P: Platform>: Send + Sync + 'static {
	/// Gets called every time this step is executed in the pipeline.
	///
	/// As an input it takes the payload that has been built so far and outputs
	/// a new payload that will be used in the next step of the pipeline, or a
	/// failure that will terminate the pipeline execution.
	fn step(
		self: Arc<Self>,
		_payload: Checkpoint<P>,
		_ctx: StepContext<P>,
	) -> impl Future<Output = ControlFlow<P>> + Send;

	/// This function is called once per new payload job before any steps are
	/// executed. It can be used by steps to perform any optional initialization
	/// of its internal state for a given payload job.
	///
	/// If this function returns an error, the pipeline execution will be
	/// terminated immediately and no steps will be executed.
	fn before_job(
		self: Arc<Self>,
		_ctx: StepContext<P>,
	) -> impl Future<Output = Result<(), PayloadBuilderError>> + Send {
		async { Ok(()) }
	}

	/// This function is called once after all steps in the pipeline have been
	/// executed. It will be called with the outcome of the step execution,
	/// which is either a successful payload or an error.
	///
	/// A failure in this function will invalidate the whole payload job
	/// and will not produce a valid payload.
	fn after_job(
		self: Arc<Self>,
		_ctx: StepContext<P>,
		_result: Arc<Result<types::BuiltPayload<P>, PayloadBuilderError>>,
	) -> impl Future<Output = Result<(), PayloadBuilderError>> + Send {
		async { Ok(()) }
	}

	/// Optional setup function called exactly once when a pipeline is
	/// instantiated as a payload builder service, before any payload jobs run.
	fn setup(
		&mut self,
		_init: InitContext<P>,
	) -> impl Future<Output = Result<(), PayloadBuilderError>> + Send {
		async { Ok(()) }
	}
}

/// This type is returned from every step in the pipeline and it controls the
/// next action of the pipeline execution.
///
/// TODO:
/// 	Consider reducing the number of variant to just `Ok` and `Break` and make
///   `ControlFlow` a `Result` type. This will allow us to use the `?` operator
///   in the step implementations and make the code more concise.
#[derive(Debug)]
pub enum ControlFlow<P: Platform> {
	/// Immediately terminate the pipeline execution with an error and
	/// no valid payload will be produced by the entire hierarchy of pipelines
	/// that contains this step.
	Fail(PayloadBuilderError),

	/// Stops the pipeline execution that contains the step with a payload.
	///
	/// If the step is inside a `Loop` sub-pipeline, it will stop the loop,
	/// run its epilogue (if it exists) and progress to the next steps in the
	/// parent pipeline.
	///
	/// Breaking out of a prologue step will not invoke any step in the pipeline
	/// and jump straight to the epilogue.
	///
	/// Breaking out of an epilogue has the same effect as returning Ok from it,
	/// and will continue the pipeline execution to the next step in the parent
	/// pipeline.
	Break(Checkpoint<P>),

	/// Continues the pipeline execution to the next step with the given payload
	/// version
	Ok(Checkpoint<P>),
}

impl<P: Platform, E: core::error::Error + Send + Sync + 'static> From<E>
	for ControlFlow<P>
{
	fn from(value: E) -> Self {
		ControlFlow::Fail(PayloadBuilderError::other(value))
	}
}

impl<P: Platform> ControlFlow<P> {
	pub fn try_into_payload(self) -> Result<Checkpoint<P>, Self> {
		match self {
			ControlFlow::Ok(payload) | ControlFlow::Break(payload) => Ok(payload),
			ControlFlow::Fail(_) => Err(self),
		}
	}

	pub fn try_into_fail(self) -> Result<PayloadBuilderError, Self> {
		match self {
			ControlFlow::Fail(err) => Ok(err),
			_ => Err(self),
		}
	}

	pub const fn is_break(&self) -> bool {
		matches!(self, ControlFlow::Break(_))
	}

	pub const fn is_fail(&self) -> bool {
		matches!(self, ControlFlow::Fail(_))
	}

	pub const fn is_ok(&self) -> bool {
		matches!(self, ControlFlow::Ok(_))
	}
}

/// Context for the optional setup function of a step.
#[derive(Clone)]
pub struct InitContext<P: Platform> {
	metrics_scope: String,
	provider: Arc<dyn StateProviderFactory>,
	_phantom: PhantomData<P>,
}

impl<P: Platform> InitContext<P> {
	/// Private API, called from [`PipelineServiceBuilder`].
	pub(crate) fn new(
		provider: Arc<dyn StateProviderFactory>,
		metrics_scope: String,
	) -> Self {
		Self {
			provider,
			metrics_scope,
			_phantom: PhantomData,
		}
	}
}

impl<P: Platform> InitContext<P> {
	/// Access to the node state
	pub fn provider(&self) -> &dyn StateProviderFactory {
		&*self.provider
	}

	/// Metrics scope assigned to this step. Steps that want to produce their own
	/// custom metrics should use this scope.
	pub fn metrics_scope(&self) -> &str {
		&self.metrics_scope
	}
}
