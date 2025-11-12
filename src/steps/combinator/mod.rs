//! Combinators provide different execution semantics for composing multiple
//! steps. Order of insertion is important and may affect the final execution
//! result.
//!
//! # Combinator Types
//!
//! - [`Chain`]: Executes steps sequentially. If any step returns
//!   [`ControlFlow::Break`] or [`ControlFlow::Fail`], execution stops
//!   immediately and that result is returned. This is similar to the normal
//!   pipeline execution semantics with a succession of `with_step` calls.
//!
//! - [`Atomic`]: Executes steps sequentially but with rollback semantics. If
//!   any step returns [`ControlFlow::Break`] or [`ControlFlow::Fail`], the
//!   entire sequence is rolled back to the initial checkpoint state.
//!
//! # Examples
//!
//! ```rust
//! use rblib::prelude::*;
//!
//! # fn example<P: Platform>(step1: impl Step<P>, step2: impl Step<P>, step3: impl Step<P>) {
//! // Create an `Chain` combinator that executes steps sequentially
//! let all_steps = Chain::of(step1).then(step2).then(step3);
//!
//! // Create an Atomic combinator that has rollback properties
//! let atomic_steps = Atomic::of(step1).and(step2).and(step3);
//! # }
//! ```

use {crate::prelude::*, std::sync::Arc};

type Steps<P: Platform> = Vec<Arc<StepInstance<P>>>;

/// Trait for combinator steps that can be constructed from multiple steps
///
/// This trait provides the `of` method for creating combinators from any type
/// that implements [`IntoSteps`]. It also requires `Step` to be implemented.
/// See the ` combinator ` macro for a base implementation with an underlying
/// `Vec` collection of steps.
pub trait CombinatorStep<P: Platform>: Step<P> {
	/// Creates a new combinator from a collection of steps.
	fn of(steps: impl IntoSteps<P>) -> Self;
}

/// Internal macro to generate default combinator step implementation.
/// Provides:
/// - implementation of [`CombinatorStep`] for a struct named `$name`
/// - `$append()` method to append a step to the collection
/// - `steps()` method to get a slice of the steps for the `Step` implementation
macro_rules! combinator {
	// name is the combinator struct name,
	// append the method name of adding a step (optional)
	($name:ident, $append:ident) => {
		/// Combinator step that executes a sequence of steps in order.
		pub struct $name<P: Platform>(pub Steps<P>);

		impl<P: Platform> CombinatorStep<P> for $name<P> {
			fn of(steps: impl IntoSteps<P>) -> Self {
				Self(steps.into_steps())
			}
		}

		impl<P: Platform> $name<P> {
			/// Appends a step to the combinator.
			#[must_use]
			pub fn $append(mut self, other: impl IntoSteps<P>) -> Self {
				self.0.extend(other.into_steps());
				self
			}

			/// Returns a slice of the steps in the combinator.
			pub fn steps(&self) -> &[Arc<StepInstance<P>>] {
				&self.0
			}
		}

		// TODO: if macro_metavar_expr feature is stabilized, we can generate macro
		// automatically: #[macro_export]
		// macro_rules! $macro_name {
		//     ($$first:expr $(, $$rest:expr)* $(,)?) => {{
		//         let mut c =
		//             $name::of($$first);
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

mod atomic;
mod chain;

/// Public API
pub use {atomic::Atomic, chain::Chain};

/// Trait for converting types into a collection of steps.
///
/// This trait is automatically implemented for:
/// - Any type implementing [`Step`]
/// - TODO: combinators for tuples of steps up to 32-128 elements
pub trait IntoSteps<P: Platform> {
	/// Converts self into a collection of steps.
	#[track_caller]
	fn into_steps(self) -> Steps<P>;
}

impl<P: Platform, S: Step<P>> IntoSteps<P> for S {
	#[track_caller]
	fn into_steps(self) -> Steps<P> {
		vec![Arc::new(StepInstance::new(self))]
	}
}

#[cfg(test)]
pub mod tests {
	use {super::*, crate::test_utils::*};

	combinator!(TestCombinator, test_append);
	impl<P: Platform> Step<P> for TestCombinator<P> {
		async fn step(
			self: Arc<Self>,
			_: Checkpoint<P>,
			_: StepContext<P>,
		) -> ControlFlow<P> {
			unimplemented!("Step `{}` is not implemented", stringify!($name))
		}
	}

	fake_step!(Step1);
	fake_step!(Step2);

	#[rblib_test(Ethereum, Optimism)]
	async fn step_into_steps<P: TestablePlatform>() {
		let steps: Steps<P> = Step1.into_steps();
		assert_eq!(steps.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn combinator_step_macro<P: TestablePlatform>() {
		let combinator = TestCombinator::<P>::of(Step1);
		assert_eq!(combinator.steps().len(), 1);
		assert_eq!(combinator.steps()[0].name(), "Step1");

		let combinator = combinator.test_append(Step2);
		assert_eq!(combinator.steps().len(), 2);
		assert_eq!(combinator.steps()[1].name(), "Step2");
	}
}
