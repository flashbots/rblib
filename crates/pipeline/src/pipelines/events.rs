use {
	super::*,
	core::{any::TypeId, marker::PhantomData},
	dashmap::{DashMap, mapref::one::Ref},
	tokio::sync::broadcast::Sender,
	tokio_stream::{StreamExt, wrappers::BroadcastStream},
};

/// This type is responsible for propagating events emitted by steps in the
/// pipeline.
///
/// Notes:
///  - In a pipeline with nested pipelines, the top-level pipeline's event bus
///    is responsible for handling all events of all contained pipelines and
///    steps.
#[derive(Default, Debug)]
pub(super) struct EventsBus<P: Platform> {
	publishers: DashMap<TypeId, Sender<Arc<dyn Any + Send + Sync>>>,
	phantom: PhantomData<P>,
}

impl<P: Platform> EventsBus<P> {
	/// Publishes an event of type `E` to all current subscribers.
	/// Each subscriber will receive a clone of the event.
	pub(super) fn publish<E>(&self, event: E)
	where
		E: Clone + Any + Send + Sync + 'static,
	{
		let _ = self.sender::<E>().send(Arc::new(event));
	}

	/// Returns a stream that yields events of type `E`.
	pub(super) fn subscribe<E>(
		&self,
	) -> impl Stream<Item = E> + Send + Sync + 'static
	where
		E: Clone + Any + Send + Sync + 'static,
	{
		BroadcastStream::new(self.sender::<E>().subscribe()).filter_map(|event| {
			let event = event.ok()?;
			let casted = event.downcast::<E>().ok()?;
			Some((*casted).clone())
		})
	}

	fn sender<E>(&self) -> Ref<'_, TypeId, Sender<Arc<dyn Any + Send + Sync>>>
	where
		E: Clone + Any + Send + Sync + 'static,
	{
		let key = TypeId::of::<E>();

		// the most likely case is that the sender already exists, so we
		// avoid using `.entry()` here to avoid taking a write lock on the map.
		if let Some(sender) = self.publishers.get(&key) {
			return sender;
		}

		// otherwise, we create a new sender for this event type, insert it into the
		// map and immediately downgrade the lock to a read lock.
		self
			.publishers
			.entry(TypeId::of::<E>())
			.or_insert_with(|| Sender::new(100))
			.downgrade()
	}
}

/// System events emitted by the pipeline itself.
pub(super) mod system_events {
	use {
		super::*,
		derive_more::{Deref, From, Into},
		reth_payload_builder::PayloadId,
	};

	/// System event emitted when a new payload job is started.
	#[derive(Debug, Clone, From, Into, Deref)]
	pub struct PayloadJobStarted<P: Platform>(pub BlockContext<P>);

	/// System event emitted when a payload job has successfully built a payload.
	#[derive(Debug, Clone)]
	pub struct PayloadJobCompleted<P: Platform> {
		pub payload_id: PayloadId,
		pub built_payload: types::BuiltPayload<P>,
	}

	/// System event emitted when a payload job has failed to build a payload.
	#[derive(Debug, Clone)]
	pub struct PayloadJobFailed {
		pub payload_id: PayloadId,
		pub error: Arc<PayloadBuilderError>,
	}

	/// System event emitted when the pipeline instance is dropped.
	#[derive(Debug, Clone)]
	pub struct PipelineDropped;
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::{alloy, reth, test_utils::*},
		alloy::consensus::BlockHeader,
		futures::StreamExt,
		reth::node::builder::BuiltPayload,
	};

	#[derive(Clone, Debug, PartialEq, Eq)]
	struct UInt32Event(u32);

	struct StepEmittingOneTypeOfEvent;
	impl<P: Platform> Step<P> for StepEmittingOneTypeOfEvent {
		async fn before_job(
			self: Arc<Self>,
			ctx: StepContext<P>,
		) -> Result<(), PayloadBuilderError> {
			ctx.emit(StringEvent("before job".to_string()));
			Ok(())
		}

		async fn step(
			self: Arc<Self>,
			payload: Checkpoint<P>,
			ctx: StepContext<P>,
		) -> ControlFlow<P> {
			ctx.emit(StringEvent("inside step".to_string()));
			ControlFlow::Ok(payload)
		}

		async fn after_job(
			self: Arc<Self>,
			ctx: StepContext<P>,
			_: Arc<Result<types::BuiltPayload<P>, PayloadBuilderError>>,
		) -> Result<(), PayloadBuilderError> {
			ctx.emit(StringEvent("after job".to_string()));
			Ok(())
		}
	}

	struct StepEmittingTwoTypesOfEvents;
	impl<P: Platform> Step<P> for StepEmittingTwoTypesOfEvents {
		async fn before_job(
			self: Arc<Self>,
			ctx: StepContext<P>,
		) -> Result<(), PayloadBuilderError> {
			ctx.emit(StringEvent("before job".to_string()));
			ctx.emit(UInt32Event(50));
			Ok(())
		}

		async fn step(
			self: Arc<Self>,
			payload: Checkpoint<P>,
			ctx: StepContext<P>,
		) -> ControlFlow<P> {
			ctx.emit(StringEvent("inside step".to_string()));
			ctx.emit(UInt32Event(100));
			ctx.emit(UInt32Event(101));
			ControlFlow::Ok(payload)
		}

		async fn after_job(
			self: Arc<Self>,
			ctx: StepContext<P>,
			_: Arc<Result<types::BuiltPayload<P>, PayloadBuilderError>>,
		) -> Result<(), PayloadBuilderError> {
			ctx.emit(StringEvent("after job".to_string()));
			ctx.emit(UInt32Event(200));
			Ok(())
		}
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn one_event_type_is_propagated<P: TestablePlatform>() {
		let step = OneStep::<P>::new(StepEmittingOneTypeOfEvent);

		let mut string_subs = step.subscribe::<StringEvent>();

		let output = step.run().await;
		assert!(output.is_ok());

		assert_eq!(
			string_subs.next().await,
			Some(StringEvent("before job".to_string()))
		);

		assert_eq!(
			string_subs.next().await,
			Some(StringEvent("inside step".to_string()))
		);

		assert_eq!(
			string_subs.next().await,
			Some(StringEvent("after job".to_string()))
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn two_event_types_are_propagated<P: TestablePlatform>() {
		let step = OneStep::<P>::new(StepEmittingTwoTypesOfEvents);

		let mut string_subs = step.subscribe::<StringEvent>();
		let mut uint32_subs = step.subscribe::<UInt32Event>();

		let output = step.run().await;
		assert!(output.is_ok());

		assert_eq!(
			string_subs.next().await,
			Some(StringEvent("before job".to_string()))
		);

		assert_eq!(
			string_subs.next().await,
			Some(StringEvent("inside step".to_string()))
		);

		assert_eq!(
			string_subs.next().await,
			Some(StringEvent("after job".to_string()))
		);
		assert!(string_subs.next().await.is_none());

		assert_eq!(uint32_subs.next().await, Some(UInt32Event(50)));
		assert_eq!(uint32_subs.next().await, Some(UInt32Event(100)));
		assert_eq!(uint32_subs.next().await, Some(UInt32Event(101)));
		assert_eq!(uint32_subs.next().await, Some(UInt32Event(200)));
		assert!(uint32_subs.next().await.is_none());
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn system_events_are_emitted_on_success<P: TestablePlatform>() {
		let step = OneStep::<P>::new(AlwaysOkStep);

		let mut started_sub = step.subscribe::<PayloadJobStarted<P>>();
		let mut completed_sub = step.subscribe::<PayloadJobCompleted<P>>();
		let mut failed_sub = step.subscribe::<PayloadJobFailed>();
		let mut dropped_sub = step.subscribe::<PipelineDropped>();

		let output = step.run().await.unwrap();
		let ControlFlow::Ok(checkpoint) = output else {
			panic!("Expected ControlFlow::Ok, got: {output:?}");
		};

		let started_event = started_sub.next().await;
		assert!(started_event.is_some());
		let started_event = started_event.unwrap();

		assert_eq!(started_event.payload_id(), checkpoint.block().payload_id());
		assert_eq!(started_event.parent(), checkpoint.block().parent());
		assert_eq!(started_event.number(), checkpoint.block().number());

		let completed_event = completed_sub.next().await;
		assert!(completed_event.is_some());
		let completed_event = completed_event.unwrap();

		assert_eq!(completed_event.payload_id, checkpoint.block().payload_id());
		assert_eq!(
			completed_event.built_payload.block().header().parent_hash(),
			checkpoint.block().parent().hash()
		);

		assert!(failed_sub.next().await.is_none());
		assert!(dropped_sub.next().await.is_some());
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn system_events_are_emitted_on_fail<P: TestablePlatform>() {
		let step = OneStep::<P>::new(AlwaysFailStep);

		let mut started_sub = step.subscribe::<PayloadJobStarted<P>>();
		let mut completed_sub = step.subscribe::<PayloadJobCompleted<P>>();
		let mut failed_sub = step.subscribe::<PayloadJobFailed>();
		let mut dropped_sub = step.subscribe::<PipelineDropped>();

		let output = step.run().await.unwrap();
		let ControlFlow::Fail(error) = output else {
			panic!("Expected ControlFlow::Fail, got: {output:?}");
		};

		let started_event = started_sub.next().await;
		assert!(started_event.is_some());
		let started_event = started_event.unwrap();
		assert_eq!(started_event.number(), 1);

		let payload_id = started_event.payload_id();

		let failed_event = failed_sub.next().await;
		assert!(failed_event.is_some());
		let failed_event = failed_event.unwrap();

		assert_eq!(failed_event.payload_id, payload_id);
		assert_eq!(failed_event.error.to_string(), error.to_string());

		assert!(completed_sub.next().await.is_none());
		assert!(dropped_sub.next().await.is_some());
	}
}
