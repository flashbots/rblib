use {super::*, crate::platform::types::BuiltPayload};

combinator!(
	/// The [`Atomic`] combinator executes a sequence of steps with atomic
	/// (all-or-nothing) semantics. If any step fails or breaks, the entire
	/// sequence rolls back to the initial checkpoint state.
	///
	/// # Execution Semantics
	///
	/// - If all steps return [`ControlFlow::Ok`], the final checkpoint is returned
	/// - If any step returns [`ControlFlow::Break`], the initial checkpoint is
	///   returned
	/// - If any step returns [`ControlFlow::Fail`], the initial checkpoint is
	///   returned
	/// - The checkpoint is saved at the beginning and restored if any step doesn't
	///   return Ok
	///
	/// At any point, if the deadline is reached, execution stops and returns
	/// [`ControlFlow::Break`] with the initial checkpoint.
	///
	/// # Example
	///
	/// ```rust
	/// use rblib::prelude::*;
	///
	/// # fn example<P: Platform>(
	/// #     step1: impl Step<P>,
	/// #     step2: impl Step<P>,
	/// #     step3: impl Step<P>
	/// # ) {
	/// let atomic = Atomic::of(step1).and(step2).and(step3);
	/// let atomic = atomic!(step1, step2, step3);
	/// # }
	/// ```
	, Atomic, and
);

impl<P: Platform> Step<P> for Atomic<P> {
	async fn before_job(
		self: Arc<Self>,
		ctx: StepContext<P>,
	) -> Result<(), PayloadBuilderError> {
		for step in self.steps() {
			step.before_job(ctx.clone()).await?;
		}
		Ok(())
	}

	async fn after_job(
		self: Arc<Self>,
		ctx: StepContext<P>,
		result: Arc<Result<BuiltPayload<P>, PayloadBuilderError>>,
	) -> Result<(), PayloadBuilderError> {
		for step in self.steps() {
			step.after_job(ctx.clone(), result.clone()).await?;
		}
		Ok(())
	}

	async fn setup(
		&mut self,
		init: InitContext<P>,
	) -> Result<(), PayloadBuilderError> {
		for step in self.steps() {
			step.setup(init.clone()).await?;
		}
		Ok(())
	}

	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		ctx: StepContext<P>,
	) -> ControlFlow<P> {
		let initial = payload.clone();
		let mut current = payload;

		for step in self.steps() {
			if ctx.deadline_reached() {
				return ControlFlow::Ok(initial);
			}

			match step.step(current, ctx.clone()).await {
				ControlFlow::Ok(next) => current = next,
				_ => return ControlFlow::Ok(initial),
			}
		}

		if ctx.deadline_reached() {
			ControlFlow::Ok(initial)
		} else {
			ControlFlow::Ok(current)
		}
	}
}

#[macro_export]
macro_rules! atomic {
    ($first:expr $(, $rest:expr)* $(,)?) => {{
        let mut c = Atomic::of($first);
        $(
            c = c.and($rest);
        )*
        c
    }};
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::{
			alloy::network::TransactionBuilder,
			platform::{Ethereum, Optimism},
			steps::{CombinatorStep, RemoveRevertedTransactions},
			test_utils::*,
		},
		futures::StreamExt,
	};

	fake_step!(OkEvent2, emit_events, noop_ok);

	#[rblib_test(Ethereum, Optimism)]
	async fn atomic_ok_one_step<P: TestablePlatform>() {
		let atomic = Atomic::of(OkWithEventStep);

		let step = OneStep::<P>::new(atomic);
		let mut event_sub = step.subscribe::<StringEvent>();

		let output = step.run().await.unwrap();
		assert!(output.is_ok());

		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: before_job".to_string()))
		);
		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: step".to_string()))
		);
		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: after_job".to_string()))
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn atomic_ok_execute_in_order<P: TestablePlatform>() {
		let atomic = Atomic::of(OkWithEventStep).and(OkEvent2);

		let step = OneStep::<P>::new(atomic);
		let mut event_sub = step.subscribe::<StringEvent>();

		let output = step.run().await.unwrap();
		assert!(output.is_ok());

		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: before_job".to_string()))
		);
		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkEvent2: before_job".to_string()))
		);

		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: step".to_string()))
		);
		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkEvent2: step".to_string()))
		);

		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: after_job".to_string()))
		);
		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkEvent2: after_job".to_string()))
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn atomic_break_reverts_to_initial<P: TestablePlatform>() {
		let atomic =
			Atomic::of(RemoveRevertedTransactions::default()).and(AlwaysBreakStep);

		let output = OneStep::<P>::new(atomic)
			.with_payload_tx(|tx| tx.reverting().with_default_signer().with_nonce(0))
			.run()
			.await
			.unwrap();

		let ControlFlow::Ok(payload) = output else {
			panic!("Expected Ok payload (reverted), got: {output:?}");
		};

		// The reverting tx from the initial payload should not be removed
		assert_eq!(payload.history().transactions().count(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn atomic_fail_reverts_to_initial<P: TestablePlatform>() {
		let atomic =
			Atomic::of(RemoveRevertedTransactions::default()).and(AlwaysFailStep);

		let output = OneStep::<P>::new(atomic)
			.with_payload_tx(|tx| tx.reverting().with_default_signer().with_nonce(0))
			.run()
			.await
			.unwrap();

		let ControlFlow::Ok(payload) = output else {
			panic!("Expected Ok payload (reverted), got: {output:?}");
		};

		// The reverting tx from the initial payload should not be removed
		assert_eq!(payload.history().transactions().count(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn atomic_macro<P: TestablePlatform>() {
		let atomic = atomic!(AlwaysOkStep, AlwaysOkStep, AlwaysOkStep);
		let output = OneStep::<P>::new(atomic).run().await.unwrap();
		assert!(output.is_ok());
	}
}
