//! This module defines combinator steps that can be used to combine multiple
//! steps into a single step.

use {
	crate::{platform::types::BuiltPayload, prelude::*},
	std::sync::Arc,
};

type Steps<P: Platform> = Vec<Arc<StepInstance<P>>>;

pub trait CombinatorStep<P: Platform>: Step<P> {
	fn of(steps: impl Into<Steps<P>>) -> Self;
}

macro_rules! combinator {
	// name is the combinator struct name,
	// append the method name of adding a step (optional)
	($name:ident, $append:ident) => {
		pub struct $name<P: Platform>(pub Steps<P>);

		impl<P: Platform> CombinatorStep<P> for $name<P> {
			fn of(steps: impl Into<Steps<P>>) -> Self {
				Self(steps.into())
			}
		}

		impl<P: Platform> $name<P> {
			#[must_use]
			pub fn $append(mut self, other: impl Step<P>) -> Self {
				self.0.push(Arc::new(StepInstance::new(other)));
				self
			}

			pub fn steps(&self) -> &[Arc<StepInstance<P>>] {
				&self.0
			}
		}

		// TODO: if macro_metavar_expr feature is stabilized, we can generate macro
		// automatically: #[macro_export]
		// macro_rules! $macro_name {
		//     ($$first:expr $(, $$rest:expr)* $(,)?) => {{
		//         let mut c =
		//             $name::of(vec![Arc::new(StepInstance::new($$first))]);
		//         $(
		//             c = c.$append($$rest);
		//         )*
		//         c
		//     }};
		// }
	};

	// default method name for adding a step: append
	($name:ident) => {
		combinator!($name, append);
	};
}

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
        let mut c = Atomic::of(vec![Arc::new(StepInstance::new($first))]);
        $(
            c = c.and($rest);
        )*
        c
    }};
}
