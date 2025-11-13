use {super::*, crate::platform::types::BuiltPayload};

combinator!(
	/// The [`Chain`] combinator executes a sequence of steps in order. Each step
	/// receives the output checkpoint from the previous step.
	///
	/// # Execution Semantics
	///
	/// - If a step returns [`ControlFlow::Ok`], execution continues with the next
	///   step
	/// - If a step returns [`ControlFlow::Break`], execution stops and returns
	///   [`ControlFlow::Break`]
	/// - If a step returns [`ControlFlow::Fail`], execution stops and returns
	///   [`ControlFlow::Fail`]
	/// - The final result is either the last successful checkpoint or the first
	///   non-Ok control flow
	///
	/// At any point, if the deadline is reached, execution stops and returns
	/// [`ControlFlow::Break`] with the last successful checkpoint.
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
	/// let chain = Chain::of(step1).then(step2).then(step3);
	/// // or using macro:
	/// let chain = chain!(step1, step2, step3);
	/// # }
	/// ```
	, Chain, then
);

impl<P: Platform> Step<P> for Chain<P> {
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
		let mut current = payload;

		for step in self.steps() {
			if ctx.deadline_reached() {
				return ControlFlow::Break(current);
			}

			match step.step(current, ctx.clone()).await {
				ControlFlow::Ok(next) => current = next,
				ControlFlow::Break(next) => return ControlFlow::Break(next),
				ControlFlow::Fail(err) => return ControlFlow::Fail(err),
			}
		}

		if ctx.deadline_reached() {
			ControlFlow::Break(current)
		} else {
			ControlFlow::Ok(current)
		}
	}
}

#[macro_export]
macro_rules! chain {
    ($first:expr $(, $rest:expr)* $(,)?) => {{
        let mut c = Chain::of($first);
        $(
            c = c.then($rest);
        )*
        c
    }};
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::{
			platform::{Ethereum, Optimism},
			steps::CombinatorStep,
			test_utils::*,
		},
		futures::StreamExt,
	};

	fake_step!(OkEvent2, emit_events, noop_ok);

	#[rblib_test(Ethereum, Optimism)]
	async fn chain_ok_one_step<P: TestablePlatform>() {
		let chain = Chain::of(OkWithEventStep);

		let step = OneStep::<P>::new(chain);
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
	async fn chain_execute_in_order<P: TestablePlatform>() {
		let chain = Chain::of(OkWithEventStep).then(OkEvent2);

		let step = OneStep::<P>::new(chain);
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
	async fn chain_break_stops_execution<P: TestablePlatform>() {
		let chain = Chain::of(AlwaysBreakStep).then(OkWithEventStep);

		let step = OneStep::<P>::new(chain);
		let mut event_sub = step.subscribe::<StringEvent>();

		let output = step.run().await.unwrap();
		assert!(output.is_break());

		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: before_job".to_string()))
		);
		// step is not called
		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: after_job".to_string()))
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn chain_fail_stops_execution<P: TestablePlatform>() {
		let chain = Chain::of(AlwaysFailStep).then(OkWithEventStep);

		let step = OneStep::<P>::new(chain);
		let mut event_sub = step.subscribe::<StringEvent>();

		let output = step.run().await.unwrap();
		assert!(output.is_fail());

		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: before_job".to_string()))
		);
		// step is not called
		assert_eq!(
			event_sub.next().await,
			Some(StringEvent("OkWithEventStep: after_job".to_string()))
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn chain_macro<P: TestablePlatform>() {
		let chain = chain!(AlwaysOkStep, AlwaysOkStep, AlwaysOkStep);
		let output = OneStep::<P>::new(chain).run().await.unwrap();
		assert!(output.is_ok());
	}
}
