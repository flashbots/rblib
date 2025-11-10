//! This module defines composite steps that can be used to compose multiple
//! steps into a single step.

use {
	crate::{
		pipelines::step::StepInstance,
		platform::types::BuiltPayload,
		prelude::*,
	},
	std::sync::Arc,
};

/// A composite step with a list of steps.
/// The associated mode defines the specific behavior of executing steps
pub struct CompositeStep<P: Platform, M> {
	steps: Vec<Arc<StepInstance<P>>>,
	mode: M,
}

impl<P: Platform, M> CompositeStep<P, M> {
	pub fn new(mode: M) -> Self {
		Self {
			steps: Vec::new(),
			mode,
		}
	}

	pub fn append_step(&mut self, step: impl Step<P>) {
		self.steps.push(Arc::new(StepInstance::new(step)));
	}
}

/// A composite step mode defines the specific behavior of executing steps
/// It takes the list of steps, initial payload and context
trait CompositeStepMode<P: Platform>: Send + Sync {
	fn steps(
		&self,
		steps: &[Arc<StepInstance<P>>],
		payload: Checkpoint<P>,
		ctx: StepContext<P>,
	) -> impl Future<Output = ControlFlow<P>> + Send;
}

/// `CompositeStep` will execute all before/after/setup functions of each step
/// in order The step implementation is delegated to the composite mode
impl<P, M> Step<P> for CompositeStep<P, M>
where
	P: Platform,
	M: CompositeStepMode<P> + Send + 'static,
{
	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		ctx: StepContext<P>,
	) -> ControlFlow<P> {
		self.mode.steps(&self.steps, payload, ctx).await
	}

	async fn before_job(
		self: Arc<Self>,
		ctx: StepContext<P>,
	) -> Result<(), PayloadBuilderError> {
		for step in &self.steps {
			step.before_job(ctx.clone()).await?;
		}
		Ok(())
	}

	async fn after_job(
		self: Arc<Self>,
		ctx: StepContext<P>,
		result: Arc<Result<BuiltPayload<P>, PayloadBuilderError>>,
	) -> Result<(), PayloadBuilderError> {
		for step in &self.steps {
			step.after_job(ctx.clone(), result.clone()).await?;
		}
		Ok(())
	}

	async fn setup(
		&mut self,
		init: InitContext<P>,
	) -> Result<(), PayloadBuilderError> {
		for step in &self.steps {
			step.setup(init.clone()).await?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use {super::*, crate::test_utils::*};

	// Composite step mode that tries to execute all steps and ignore all failures
	// and breaks Will return the latest successful result
	struct TryAllMode;
	impl<P: Platform> CompositeStepMode<P> for TryAllMode {
		async fn steps(
			&self,
			steps: &[Arc<StepInstance<P>>],
			payload: Checkpoint<P>,
			ctx: StepContext<P>,
		) -> ControlFlow<P> {
			let mut current = payload.clone();
			for step in steps {
				if let ControlFlow::Ok(res) =
					step.step(current.clone(), ctx.clone()).await
				{
					current = res;
				}
			}
			ControlFlow::Ok(current)
		}
	}

	#[rblib_test(Ethereum)]
	async fn composite_empty_steps<P: TestablePlatform>() -> eyre::Result<()> {
		let composite = CompositeStep::<P, _>::new(TryAllMode);
		let result = OneStep::<P>::new(composite).run().await?;
		assert!(matches!(result, ControlFlow::Ok(_)));
		Ok(())
	}

	#[rblib_test(Ethereum)]
	async fn composite_single_step_ok<P: TestablePlatform>() -> eyre::Result<()> {
		let mut composite = CompositeStep::<P, _>::new(TryAllMode);
		composite.append_step(AlwaysOkStep);

		let result = OneStep::<P>::new(composite).run().await?;
		assert!(matches!(result, ControlFlow::Ok(_)));
		Ok(())
	}

	#[rblib_test(Ethereum)]
	async fn composite_multiple_steps_ok<P: TestablePlatform>() -> eyre::Result<()>
	{
		let mut composite = CompositeStep::<P, _>::new(TryAllMode);
		composite.append_step(AlwaysOkStep);
		composite.append_step(AlwaysOkStep);
		composite.append_step(AlwaysOkStep);

		let result = OneStep::<P>::new(composite).run().await?;
		assert!(matches!(result, ControlFlow::Ok(_)));

		Ok(())
	}
}
