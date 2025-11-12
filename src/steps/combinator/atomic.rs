use {super::*, crate::platform::types::BuiltPayload};

combinator!(Atomic, and);

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
