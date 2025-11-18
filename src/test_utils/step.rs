use {
	super::*,
	crate::{alloy, prelude::*, reth},
	alloy::network::TransactionBuilder,
	core::any::Any,
	futures::Stream,
	reth::payload::builder::PayloadBuilderError,
	std::{pin::Pin, sync::Arc},
	tokio::sync::{
		Mutex,
		mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
	},
};

/// A simple `String` event that can be emitted by a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StringEvent(pub String);

macro_rules! fake_step {
	($name:ident) => {
		#[allow(unreachable_pub)]
		#[derive(Debug, Default, Clone)]
		pub struct $name;
		impl<P: $crate::prelude::Platform> $crate::prelude::Step<P> for $name {
			async fn step(
				self: std::sync::Arc<Self>,
				_: $crate::prelude::Checkpoint<P>,
				_: $crate::prelude::StepContext<P>,
			) -> $crate::prelude::ControlFlow<P> {
				unimplemented!("Step `{}` is not implemented", stringify!($name))
			}
		}
	};

	($name:ident, noop_ok) => {
		/// A step that always returns `ControlFlow::Ok` with the input payload.
		#[allow(unreachable_pub)]
		#[derive(Debug, Default, Clone)]
		pub struct $name;
		impl<P: $crate::prelude::Platform> $crate::prelude::Step<P> for $name {
			async fn step(
				self: std::sync::Arc<Self>,
				payload: $crate::prelude::Checkpoint<P>,
				_: $crate::prelude::StepContext<P>,
			) -> $crate::prelude::ControlFlow<P> {
				$crate::prelude::ControlFlow::Ok(payload)
			}
		}
	};

	($name:ident, noop_break) => {
		/// A step that always returns `ControlFlow::Break` with the input payload.
		#[allow(unreachable_pub)]
		#[derive(Debug, Default, Clone)]
		pub struct $name;
		impl<P: $crate::prelude::Platform> $crate::prelude::Step<P> for $name {
			async fn step(
				self: std::sync::Arc<Self>,
				payload: $crate::prelude::Checkpoint<P>,
				_: $crate::prelude::StepContext<P>,
			) -> $crate::prelude::ControlFlow<P> {
				$crate::prelude::ControlFlow::Break(payload)
			}
		}
	};

	($name:ident, noop_fail) => {
		/// A step that always returns `ControlFlow::Fail` with
		/// `PayloadBuilderError::Other`.
		#[allow(unreachable_pub)]
		#[derive(Debug, Default, Clone)]
		pub struct $name;

		impl<P: $crate::prelude::Platform> $crate::prelude::Step<P> for $name {
			async fn step(
				self: std::sync::Arc<Self>,
				_: $crate::prelude::Checkpoint<P>,
				_: $crate::prelude::StepContext<P>,
			) -> $crate::prelude::ControlFlow<P> {
				$crate::prelude::ControlFlow::Fail(
					$crate::prelude::PayloadBuilderError::Other("always fail".into()),
				)
			}
		}
	};

	($name:ident, emit_events, noop_ok) => {
		#[allow(unreachable_pub)]
		#[derive(Debug, Default, Clone)]
		pub struct $name;

		impl<P: $crate::prelude::Platform> $crate::prelude::Step<P> for $name {
			async fn before_job(
				self: std::sync::Arc<Self>,
				ctx: $crate::prelude::StepContext<P>,
			) -> Result<(), $crate::prelude::PayloadBuilderError> {
				ctx.emit($crate::test_utils::StringEvent(format!(
					"{}: before_job",
					stringify!($name),
				)));
				Ok(())
			}

			async fn step(
				self: std::sync::Arc<Self>,
				payload: $crate::prelude::Checkpoint<P>,
				ctx: $crate::prelude::StepContext<P>,
			) -> $crate::prelude::ControlFlow<P> {
				ctx.emit($crate::test_utils::StringEvent(format!(
					"{}: step",
					stringify!($name),
				)));
				$crate::prelude::ControlFlow::Ok(payload)
			}

			async fn after_job(
				self: std::sync::Arc<Self>,
				ctx: $crate::prelude::StepContext<P>,
				_: std::sync::Arc<
					Result<
						$crate::prelude::types::BuiltPayload<P>,
						$crate::prelude::PayloadBuilderError,
					>,
				>,
			) -> Result<(), $crate::prelude::PayloadBuilderError> {
				ctx.emit($crate::test_utils::StringEvent(format!(
					"{}: after_job",
					stringify!($name),
				)));
				Ok(())
			}
		}
	};

	($name:ident, $state:ident) => {
		#[allow(unreachable_pub)]
		#[derive(Debug, Clone)]
		pub struct $name($state);
		impl<P: $crate::prelude::Platform> $crate::prelude::Step<P> for $name {
			async fn step(
				self: std::sync::Arc<Self>,
				_: $crate::prelude::Checkpoint<P>,
				_: $crate::prelude::StepContext<P>,
			) -> $crate::prelude::ControlFlow<P> {
				unimplemented!("Step `{}` is not implemented", stringify!($name))
			}
		}
	};
}

pub(crate) use fake_step;

fake_step!(AlwaysOkStep, noop_ok);
fake_step!(AlwaysBreakStep, noop_break);
fake_step!(AlwaysFailStep, noop_fail);

fake_step!(OkWithEventStep, emit_events, noop_ok);

/// This test util is used in unit tests for testing a single step in isolation.
///
/// It allows running a single step with a predefined list of transactions in
/// the payload as an input and returns the output of the step control flow
/// result.
///
/// The step is invoked with full node facilities and in realistic condition.
pub struct OneStep<P: PlatformWithRpcTypes> {
	pipeline: Pipeline<P>,
	pool_txs: Vec<BoxedTxBuilderFn<P>>,
	payload_input: Vec<InputPayloadItemFn<P>>,
	payload_tx: UnboundedSender<InputPayloadItem<P>>,
	ok_rx: UnboundedReceiver<Checkpoint<P>>,
	fail_rx: UnboundedReceiver<PayloadBuilderError>,
	break_rx: UnboundedReceiver<Checkpoint<P>>,
}

impl<P: PlatformWithRpcTypes + TestNodeFactory<P>> OneStep<P> {
	#[track_caller]
	pub fn new(step: impl Step<P>) -> Self {
		let (prepopulate, payload_tx) = PopulatePayload::new();
		let (record_ok, ok_rx) = RecordOk::new();
		let (record_fail, fail_rx, break_rx) = RecordBreakAndFail::new();

		let pipeline = Pipeline::<P>::default()
			.with_step(prepopulate)
			.with_step(step)
			.with_step(record_ok)
			.with_epilogue(record_fail);

		Self {
			pipeline,
			payload_input: Vec::new(),
			pool_txs: Vec::new(),
			payload_tx,
			ok_rx,
			fail_rx,
			break_rx,
		}
	}

	#[must_use]
	pub fn with_limits(mut self, limits: Limits<P>) -> Self {
		self.pipeline = self.pipeline.with_limits(limits);
		self
	}

	/// Adds a new transaction to the input payload of the step.
	///
	/// Note that transactions added through this method will not go through the
	/// mempool and instead directly into the payload of the step.
	/// Nonces need to be set manually because they will not be reported by the
	/// mempool "pending" transactions count
	#[must_use]
	pub fn with_payload_tx(
		mut self,
		builder: impl FnMut(
			types::TransactionRequest<P>,
		) -> types::TransactionRequest<P>
		+ 'static,
	) -> Self {
		self
			.payload_input
			.push(InputPayloadItemFn::Tx(Box::new(builder)));
		self
	}

	/// Adds a new bundle to the input payload of the step.
	#[must_use]
	pub fn with_payload_bundle(mut self, bundle: types::Bundle<P>) -> Self {
		self.payload_input.push(InputPayloadItemFn::Bundle(bundle));
		self
	}

	/// Adds a barrier to the input payload of the step at the current position.
	#[must_use]
	pub fn with_payload_barrier(mut self) -> Self {
		self.payload_input.push(InputPayloadItemFn::Barrier);
		self
	}

	/// Adds a new transaction to the mempool and makes it available to the step.
	/// Here we don't need to manage nonces, as the mempool will report the
	/// pending transactions for the signer and nonces will be set automatically.
	#[must_use]
	pub fn with_pool_tx(
		mut self,
		builder: impl FnMut(
			types::TransactionRequest<P>,
		) -> types::TransactionRequest<P>
		+ 'static,
	) -> Self {
		self.pool_txs.push(Box::new(builder));
		self
	}

	/// Subscribes to events of type `E` emitted by the step.
	pub fn subscribe<E>(&self) -> impl Stream<Item = E> + Send + Sync + 'static
	where
		E: Clone + Any + Send + Sync + 'static,
	{
		self.pipeline.subscribe::<E>()
	}

	/// Runs a single invocation of the step with the prepared environment and
	/// returns the control flow result of the step execution.
	pub fn run(
		mut self,
	) -> Pin<Box<dyn Future<Output = eyre::Result<ControlFlow<P>>>>> {
		Box::pin(async move {
			let local_node = P::create_test_node(self.pipeline).await?;
			let input_txs = self
				.payload_input
				.into_iter()
				.map(|input| -> eyre::Result<InputPayloadItem<P>> {
					Ok(match input {
						InputPayloadItemFn::Barrier => InputPayloadItem::Barrier,
						InputPayloadItemFn::Tx(mut builder) => InputPayloadItem::Tx(
							builder(
								local_node
									.build_tx()
									.with_max_fee_per_gas(2_000_000_001)
									.with_max_priority_fee_per_gas(1),
							)
							.build_with_known_signer()?,
						),
						InputPayloadItemFn::Bundle(bundle) => {
							InputPayloadItem::Bundle(bundle)
						}
					})
				})
				.collect::<Result<Vec<_>, _>>()?;

			for tx in input_txs {
				self.payload_tx.send(tx)?;
			}

			let pool_txs = self
				.pool_txs
				.into_iter()
				.map(|mut tx| tx(local_node.build_tx()))
				.collect::<Vec<_>>();

			for tx in pool_txs {
				let _ = local_node.send_tx(tx).await?;
			}

			let _ = local_node.next_block().await;

			let ok_res = self.ok_rx.try_recv();
			let break_res = self.break_rx.try_recv();
			let fail_res = self.fail_rx.try_recv();

			if let Ok(ok) = ok_res {
				return Ok(ControlFlow::Ok(ok));
			}

			if let Ok(fail_res) = fail_res {
				return Ok(ControlFlow::Fail(fail_res));
			}

			Ok(ControlFlow::Break(break_res?))
		})
	}
}

struct PopulatePayload<P: PlatformWithRpcTypes> {
	receiver: Mutex<UnboundedReceiver<InputPayloadItem<P>>>,
}

impl<P: PlatformWithRpcTypes> PopulatePayload<P> {
	pub(crate) fn new() -> (Self, UnboundedSender<InputPayloadItem<P>>) {
		let (sender, receiver) = unbounded_channel();
		(
			Self {
				receiver: Mutex::new(receiver),
			},
			sender,
		)
	}
}

impl<P: PlatformWithRpcTypes> Step<P> for PopulatePayload<P> {
	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		_: StepContext<P>,
	) -> ControlFlow<P> {
		let mut payload = payload;
		while let Ok(input) = self.receiver.lock().await.try_recv() {
			payload = match input {
				InputPayloadItem::Barrier => payload.barrier(),
				InputPayloadItem::Tx(tx) => {
					payload.apply(tx).expect("Failed to apply transaction")
				}
				InputPayloadItem::Bundle(bundle) => {
					payload.apply(bundle).expect("Failed to apply bundle")
				}
			};
		}

		ControlFlow::Ok(payload)
	}
}

struct RecordOk<P: Platform> {
	sender: UnboundedSender<Checkpoint<P>>,
}

impl<P: Platform> RecordOk<P> {
	pub(crate) fn new() -> (Self, UnboundedReceiver<Checkpoint<P>>) {
		let (sender, receiver) = unbounded_channel();
		(Self { sender }, receiver)
	}
}

impl<P: Platform> Step<P> for RecordOk<P> {
	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		_: StepContext<P>,
	) -> ControlFlow<P> {
		self.sender.send(payload.clone()).unwrap();
		ControlFlow::Ok(payload)
	}
}

struct RecordBreakAndFail<P: Platform> {
	fail_sender: UnboundedSender<PayloadBuilderError>,
	break_sender: UnboundedSender<Checkpoint<P>>,
}

impl<P: Platform> RecordBreakAndFail<P> {
	pub(crate) fn new() -> (
		Self,
		UnboundedReceiver<PayloadBuilderError>,
		UnboundedReceiver<Checkpoint<P>>,
	) {
		let (fail_sender, fail_receiver) = unbounded_channel();
		let (break_sender, break_receiver) = unbounded_channel();
		(
			Self {
				fail_sender,
				break_sender,
			},
			fail_receiver,
			break_receiver,
		)
	}
}

impl<P: Platform> Step<P> for RecordBreakAndFail<P> {
	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		_: StepContext<P>,
	) -> ControlFlow<P> {
		self.break_sender.send(payload.clone()).unwrap();
		ControlFlow::Ok(payload)
	}

	async fn after_job(
		self: Arc<Self>,
		_: StepContext<P>,
		result: Arc<Result<types::BuiltPayload<P>, PayloadBuilderError>>,
	) -> Result<(), PayloadBuilderError> {
		if let Err(e) = result.as_ref() {
			self.fail_sender.send(clone_payload_error_lossy(e)).unwrap();
		}
		Ok(())
	}
}

enum InputPayloadItemFn<P: PlatformWithRpcTypes> {
	Barrier,
	Tx(BoxedTxBuilderFn<P>),
	Bundle(types::Bundle<P>),
}

enum InputPayloadItem<P: PlatformWithRpcTypes> {
	Barrier,
	Tx(types::TxEnvelope<P>),
	Bundle(types::Bundle<P>),
}

type BoxedTxBuilderFn<P> =
	Box<dyn FnMut(types::TransactionRequest<P>) -> types::TransactionRequest<P>>;

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn break_is_recorded() {
		let step = OneStep::<Ethereum>::new(AlwaysBreakStep)
			.run()
			.await
			.unwrap();
		assert!(matches!(step, ControlFlow::Break(_)));
	}

	#[tokio::test]
	async fn ok_is_recorded() {
		let step = OneStep::<Ethereum>::new(AlwaysOkStep).run().await.unwrap();
		assert!(matches!(step, ControlFlow::Ok(_)));
	}

	#[tokio::test]
	async fn fail_is_recorded() {
		let step = OneStep::<Ethereum>::new(AlwaysFailStep)
			.run()
			.await
			.unwrap();
		assert!(matches!(step, ControlFlow::Fail(_)));
	}
}
