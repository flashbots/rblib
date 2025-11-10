use {
	super::super::{events::EventsBus, exec::navi::StepNavigator},
	crate::{
		prelude::*,
		reth::{primitives::SealedHeader, providers::StateProvider},
	},
	core::any::Any,
	futures::Stream,
	std::{sync::Arc, time::Instant},
};

/// Carries information specific to the step that is currently being executed.
///
/// An instance of this type is passed to the [`Step::step`] method during the
/// pipeline execution of steps.
#[derive(Debug, Clone)]
pub struct StepContext<P: Platform> {
	block: BlockContext<P>,
	limits: Limits,
	events_bus: Arc<EventsBus<P>>,
	started_at: Option<Instant>,
}

impl<P: Platform> StepContext<P> {
	pub(crate) fn new(
		block: &BlockContext<P>,
		step: &StepNavigator<P>,
		limits: Limits,
		in_scope_since: Option<Instant>,
	) -> Self {
		let block = block.clone();
		let events_bus = Arc::clone(&step.root_pipeline().events);
		let started_at = in_scope_since;

		Self {
			block,
			limits,
			events_bus,
			started_at,
		}
	}

	/// Access to the state of the chain at the beginning of block that we are
	/// building. This state does not include any changes made by the pipeline
	/// during the payload building process. It does however include changes
	/// applied by platform-specific [`BlockBuilder::apply_pre_execution_changes`]
	/// for this block.
	pub fn provider(&self) -> &dyn StateProvider {
		self.block.base_state()
	}

	/// Parent block header of the block that we are building.
	pub fn parent(&self) -> &SealedHeader<types::Header<P>> {
		self.block.parent()
	}

	/// Access to the block context of the block that we are building.
	pub const fn block(&self) -> &BlockContext<P> {
		&self.block
	}

	/// Payload limits for the scope of the step.
	pub const fn limits(&self) -> &Limits {
		&self.limits
	}

	/// Returns the instant when the scope of this step was last entered.
	pub const fn in_scope_since(&self) -> Option<Instant> {
		self.started_at
	}

	/// Checks if the scope of this step has been running longer than the deadline
	/// specified in the limits. If the limits do not specify any deadline this
	/// will return false.
	pub fn deadline_reached(&self) -> bool {
		if let (Some(deadline), Some(started_at)) =
			(self.limits.deadline, self.started_at)
		{
			started_at.elapsed() >= deadline
		} else {
			false
		}
	}

	/// Broadcasts an event to all subscribers.
	pub fn emit<E>(&self, event: E)
	where
		E: Clone + Any + Send + Sync + 'static,
	{
		self.events_bus.publish(event);
	}

	/// Returns a stream that yields events of type `E` whenever they are emitted
	/// anywhere.
	pub fn subscribe<E>(&self) -> impl Stream<Item = E> + Send + Sync + 'static
	where
		E: Clone + Any + Send + Sync + 'static,
	{
		self.events_bus.subscribe::<E>()
	}
}
