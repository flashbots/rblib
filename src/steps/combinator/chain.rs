use {super::*, crate::platform::types::BuiltPayload};

combinator!(Chain, then);

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
        let mut c = Atomic::of($first);
        $(
            c = c.and($rest);
        )*
        c
    }};
}
