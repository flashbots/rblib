//! Builder Pipelines API
//!
//! This API is used to construct payload builders workflows.

pub(crate) use step::StepInstance;
use {
	crate::{prelude::*, reth::builder::components::PayloadServiceBuilder},
	core::{any::Any, fmt::Display, panic::Location},
	events::EventsBus,
	exec::navi::StepPath,
	futures::Stream,
	pipelines_macros::impl_into_pipeline_steps,
	std::sync::Arc,
};

mod events;
mod exec;
mod iter;
mod job;
mod limits;
mod metrics;
mod service;
mod step;

#[cfg(test)]
mod tests;

// public API exports
pub use {
	Behavior::{Loop, Once},
	events::system_events::*,
	limits::*,
	step::{ControlFlow, InitContext, PayloadBuilderError, Step, StepContext},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
	Once,
	Loop,
}

pub struct Pipeline<P: Platform> {
	epilogue: Option<Arc<StepInstance<P>>>,
	prologue: Option<Arc<StepInstance<P>>>,
	steps: Vec<StepOrPipeline<P>>,
	limits: Option<Arc<dyn ScopedLimits<P>>>,
	name: String,
	events: Arc<EventsBus<P>>,
}

impl<P: Platform> Default for Pipeline<P> {
	/// Creates a new empty unnamed pipeline.
	#[track_caller]
	fn default() -> Self {
		Self {
			epilogue: None,
			prologue: None,
			steps: Vec::new(),
			limits: None,
			events: Arc::default(),
			name: metrics::auto_pipeline_name(Location::caller()),
		}
	}
}

/// Pipeline Building Public API
impl<P: Platform> Pipeline<P> {
	/// Creates a new empty pipeline with a name.
	pub fn named(name: impl Into<String>) -> Self {
		let mut default = Self::default();
		default.name = name.into();
		default
	}

	/// A step that happens before any transaction is added to the block, executes
	/// as the first step in the pipeline.
	#[must_use]
	pub fn with_prologue(self, step: impl Step<P>) -> Self {
		let mut this = self;
		this.prologue = Some(Arc::new(StepInstance::new(step)));
		this
	}

	/// Conditionally add a prologue
	#[must_use]
	pub fn with_prologue_if(self, cond: bool, step: impl Step<P>) -> Self {
		if cond { self.with_prologue(step) } else { self }
	}

	/// A step that happens as the last step of the block after the whole payload
	/// has been built.
	#[must_use]
	pub fn with_epilogue(self, step: impl Step<P>) -> Self {
		let mut this = self;
		this.epilogue = Some(Arc::new(StepInstance::new(step)));
		this
	}

	/// Conditionally add an epilogue
	#[must_use]
	pub fn with_epilogue_if(self, cond: bool, step: impl Step<P>) -> Self {
		if cond { self.with_epilogue(step) } else { self }
	}

	/// A step that runs with an input that is the result of the previous step.
	/// The order of steps definition is important, as it determines the flow of
	/// data through the pipeline.
	#[must_use]
	pub fn with_step(self, step: impl Step<P>) -> Self {
		let mut this = self;
		this
			.steps
			.push(StepOrPipeline::Step(Arc::new(StepInstance::new(step))));
		this
	}

	/// Conditionally add a step
	#[must_use]
	pub fn with_step_if(self, cond: bool, step: impl Step<P>) -> Self {
		if cond { self.with_step(step) } else { self }
	}

	/// Adds a nested pipeline to the current pipeline.
	#[must_use]
	#[track_caller]
	pub fn with_pipeline<T>(
		self,
		behavior: Behavior,
		nested: impl IntoPipeline<P, T>,
	) -> Self {
		let mut this = self;
		let nested_pipeline = nested.into_pipeline();
		this
			.steps
			.push(StepOrPipeline::Pipeline(behavior, nested_pipeline));
		this
	}

	/// Sets payload building limits for the pipeline.
	///
	/// Here we can either use an instance of `LimitsFactory` that generates
	/// limits dynamically, according to a user-defined logic, or we can use a
	/// fixed `Limits` instance.
	#[must_use]
	pub fn with_limits<T, L: IntoScopedLimits<P, T>>(self, limits: L) -> Self {
		let mut this = self;
		let factory = limits.into_scoped_limits();
		this.limits = Some(Arc::new(factory) as Arc<dyn ScopedLimits<P>>);
		this
	}
}

/// Reth node integration
impl<P: Platform> Pipeline<P> {
	/// Converts the pipeline into a payload builder service instance that
	/// can be used when constructing a reth node.
	pub fn into_service<Node, Pool>(
		self,
	) -> impl PayloadServiceBuilder<Node, Pool, types::EvmConfig<P>>
	where
		Node: traits::NodeBounds<P>,
		Pool: traits::PoolBounds<P>,
	{
		service::PipelineServiceBuilder::new(self)
	}
}

/// Observability
impl<P: Platform> Pipeline<P> {
	/// Returns true if the pipeline has no steps, prologue or epilogue.
	pub fn is_empty(&self) -> bool {
		self.prologue.is_none() && self.epilogue.is_none() && self.steps.is_empty()
	}

	/// An optional name of the pipeline.
	///
	/// This is used mostly for debug printing and logging purposes.
	pub const fn name(&self) -> &str {
		self.name.as_str()
	}

	/// Subscribes to events emitted by steps in the pipeline.
	pub fn subscribe<E>(&self) -> impl Stream<Item = E> + Send + Sync + 'static
	where
		E: Clone + Any + Send + Sync + 'static,
	{
		self.events.subscribe::<E>()
	}
}

/// Internal API
impl<P: Platform> Pipeline<P> {
	pub(crate) fn prologue(&self) -> Option<&Arc<StepInstance<P>>> {
		self.prologue.as_ref()
	}

	pub(crate) fn epilogue(&self) -> Option<&Arc<StepInstance<P>>> {
		self.epilogue.as_ref()
	}

	pub(crate) fn steps(&self) -> &[StepOrPipeline<P>] {
		&self.steps
	}

	pub(crate) fn limits(&self) -> Option<&Arc<dyn ScopedLimits<P>>> {
		self.limits.as_ref()
	}

	/// Iterates over all steps in this and all nested pipelines, for each step
	/// yields its `StepPath`. Instances of `StepPath` can be used to access the
	/// step instance through [`StepPath::navigator`].
	pub(crate) fn iter_steps(&self) -> iter::StepPathIter<'_, P> {
		iter::StepPathIter::new(self)
	}
}

impl<P: Platform> Drop for Pipeline<P> {
	fn drop(&mut self) {
		self.events.publish(PipelineDropped);
	}
}

pub(crate) enum StepOrPipeline<P: Platform> {
	Step(Arc<StepInstance<P>>),
	Pipeline(Behavior, Pipeline<P>),
}

impl<P: Platform> core::fmt::Debug for StepOrPipeline<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		match self {
			StepOrPipeline::Step(step) => step.fmt(f),
			StepOrPipeline::Pipeline(behavior, pipeline) => f
				.debug_tuple("Pipeline")
				.field(behavior)
				.field(pipeline)
				.finish(),
		}
	}
}

/// This trait is used to enable various syntactic sugar for defining nested
/// pipelines
pub trait IntoPipeline<P: Platform, Marker = ()> {
	#[track_caller]
	fn into_pipeline(self) -> Pipeline<P>;
}

impl<P: Platform, F: FnOnce(Pipeline<P>) -> Pipeline<P>>
	IntoPipeline<P, Variant<0>> for F
{
	#[track_caller]
	fn into_pipeline(self) -> Pipeline<P> {
		self(Pipeline::<P>::default())
	}
}

impl<P: Platform> IntoPipeline<P, Variant<0>> for Pipeline<P> {
	#[track_caller]
	fn into_pipeline(self) -> Pipeline<P> {
		self
	}
}

impl<P: Platform, S0: Step<P>> IntoPipeline<P, Variant<0>> for (S0,) {
	#[track_caller]
	fn into_pipeline(self) -> Pipeline<P> {
		Pipeline::default().with_step(self.0)
	}
}

impl<P: Platform, S0: Step<P>> IntoPipeline<P, Variant<1>> for S0 {
	#[track_caller]
	fn into_pipeline(self) -> Pipeline<P> {
		Pipeline::default().with_step(self)
	}
}

// Generate implementations for tuples of steps up to 32 elements
#[cfg(not(feature = "long-pipelines-syntax"))]
impl_into_pipeline_steps!(32);

// Generate implementations for tuples of steps up to 512 elements.
// This is opt-in through a compile-time feature flag and in practice
// should never be needed, but it's here just in case.
#[cfg(feature = "long-pipelines-syntax")]
impl_into_pipeline_steps!(128);

/// Helper trait that supports the concise pipeline definition syntax.
pub trait PipelineBuilderExt<P: Platform> {
	fn with_prologue(self, step: impl Step<P>) -> Pipeline<P>;
	fn with_epilogue(self, step: impl Step<P>) -> Pipeline<P>;
	fn with_limits<L: ScopedLimits<P>>(self, limits: L) -> Pipeline<P>;
	fn with_name(self, name: impl Into<String>) -> Pipeline<P>;
}

impl<P: Platform, T: IntoPipeline<P, Variant<0>>> PipelineBuilderExt<P> for T {
	#[track_caller]
	fn with_prologue(self, step: impl Step<P>) -> Pipeline<P> {
		self.into_pipeline().with_prologue(step)
	}

	#[track_caller]
	fn with_epilogue(self, step: impl Step<P>) -> Pipeline<P> {
		self.into_pipeline().with_epilogue(step)
	}

	#[track_caller]
	fn with_limits<L: ScopedLimits<P>>(self, limits: L) -> Pipeline<P> {
		self.into_pipeline().with_limits(limits)
	}

	#[track_caller]
	fn with_name(self, name: impl Into<String>) -> Pipeline<P> {
		let mut pipeline = self.into_pipeline();
		pipeline.name = name.into();
		pipeline
	}
}

impl<P: Platform> Display for Pipeline<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(
			f,
			"Pipeline(name={}, {} steps)",
			self.name,
			self.steps.len()
		)
	}
}

impl<P: Platform> core::fmt::Debug for Pipeline<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		f.debug_struct("Pipeline")
			.field("name", &self.name())
			.field("prologue", &self.prologue.as_ref().map(|p| p.name()))
			.field("epilogue", &self.epilogue.as_ref().map(|p| p.name()))
			.field("steps", &self.steps)
			.field("limits", &self.limits.is_some())
			.finish_non_exhaustive()
	}
}

pub mod traits {
	use crate::{
		prelude::*,
		reth::{
			api::FullNodeTypes,
			evm::ConfigureEvm,
			node::builder::NodeTypes,
			providers::{ChainSpecProvider, HeaderProvider, StateProviderFactory},
			transaction_pool::{PoolTransaction, TransactionPool},
		},
	};

	pub trait NodeBounds<P: Platform>:
		FullNodeTypes<Types = P::NodeTypes>
	{
	}

	impl<T, P: Platform> NodeBounds<P> for T where
		T: FullNodeTypes<Types = P::NodeTypes>
	{
	}

	pub trait ProviderBounds<P: Platform>:
		StateProviderFactory
		+ ChainSpecProvider<ChainSpec = types::ChainSpec<P>>
		+ HeaderProvider<Header = types::Header<P>>
		+ Clone
		+ Send
		+ Sync
		+ 'static
	{
	}

	impl<T, P: Platform> ProviderBounds<P> for T where
		T: StateProviderFactory
			+ ChainSpecProvider<ChainSpec = types::ChainSpec<P>>
			+ HeaderProvider<Header = types::Header<P>>
			+ Clone
			+ Send
			+ Sync
			+ 'static
	{
	}

	pub trait PoolBounds<P: Platform>:
		TransactionPool<Transaction = P::PooledTransaction> + Unpin + 'static
	{
	}

	impl<T, P: Platform> PoolBounds<P> for T where
		T: TransactionPool<Transaction = P::PooledTransaction> + Unpin + 'static
	{
	}

	pub trait PooledTransactionBounds<P: Platform>:
		PoolTransaction<Consensus = types::Transaction<P>> + Send + Sync + 'static
	{
	}

	pub trait EvmConfigBounds<P: Platform>:
		ConfigureEvm<Primitives = types::Primitives<P>> + Send + Sync
	{
	}

	impl<T, P: Platform> EvmConfigBounds<P> for T where
		T: ConfigureEvm<Primitives = types::Primitives<P>> + Send + Sync
	{
	}

	pub trait PlatformExecBounds<P: Platform>:
		Platform<
			NodeTypes: NodeTypes<
				ChainSpec = types::ChainSpec<P>,
				Primitives = types::Primitives<P>,
				Payload = types::PayloadTypes<P>,
			>,
			EvmConfig = types::EvmConfig<P>,
			ExtraLimits = types::ExtraLimits<P>,
		>
	{
	}

	impl<T, P: Platform> PlatformExecBounds<T> for P where
		T: Platform<
				NodeTypes: NodeTypes<
					ChainSpec = types::ChainSpec<P>,
					Primitives = types::Primitives<P>,
					Payload = types::PayloadTypes<P>,
				>,
				EvmConfig = types::EvmConfig<P>,
				ExtraLimits = types::ExtraLimits<P>,
			>
	{
	}
}

// internal utilities
#[cfg(any(test, feature = "test-utils"))]
pub(crate) use exec::clone_payload_error_lossy;
