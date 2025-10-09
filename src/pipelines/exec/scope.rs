//! Pipeline Scopes
//!
//! # What is a pipeline scope?
//!
//! Take this pipeline as an example:
//!
//! ```
//! Pipeline::default()────────────────────────┐
//!   .with_prologue(PrologueStep)             │
//!   .with_step(Step1_1)                      │
//!   .with_pipeline(Loop, ──────────────┐     │
//!     Pipeline::default()              │     │
//!       .with_step(Step2_1)            s2    │
//!       .with_step(Step2_2)            │     │
//!       .with_pipeline(Loop,─────┐     │     │
//!         (                      │     │     s0
//!           Step3_1,             │     │     │
//!           Step3_2,            s2_3   │     │
//!           Step3_3              │     │     │
//!         ).with_limits(LimitsB)─┘     │     │
//!       .with_step(Step2_4)            │     │
//!       ) ─────────────────────────────┘     │
//!   .with_step(Step1_3)                      │
//!   .with_epilogue(EpilogueStep)             │
//!   .with_limits(LimitsA) ───────────────────┘
//! ```
//!
//! Here we have three scopes:
//! - `Scope0`, which is the root scope of the whole pipeline.
//! - `Scope2`, which is a child scope of `Scope0` and runs as the second step
//!   of `Scope0`.
//! - `Scope2_3`, which is a child scope of `Scope2` and runs as the third step
//!   of `Scope2`.
//!
//! ## Scope Entry and Lifetime
//!
//! - `Scope0` is entered once when the payload job starts and exited when the
//!   payload job ends (whether successfully or not).
//!
//! - `Scope2` is entered once when the second step of `Scope0` begins execution
//!   and remains active until some step returns `ControlFlow::Break`. Then it
//!   is exited and the scope of `Scope0` continues executing.
//!
//! - `Scope2_3` is entered once the third step of `Scope2` begins execution and
//!   remains active until some step inside it returns `ControlFlow::Break`.
//!   Then `Scope2` continues executing and when it reaches its third step
//!   `Scope2_3` is re-entered.

use {
	super::*,
	core::{cell::RefCell, time::Duration},
	metrics::{Counter, Histogram},
	parking_lot::RwLock,
	std::collections::HashMap,
};

/// Keeps track of the currently active pipeline execution scope relative to the
/// root top-level pipeline.
///
/// The current scope is determined by the last call to `switch_context` by
/// `PipelineExecutor::advance_cursor`.
///
/// Scopes manage:
///   - The metrics name for each pipeline and its nested pipelines
///   - Limits calculation and renewal for pipeline steps.
pub(crate) struct RootScope<P: Platform> {
	root: RwLock<Scope<P>>,
	current: RefCell<StepPath>,
}

impl<P: Platform> RootScope<P> {
	/// Initialize all scopes in a given top-level pipeline.
	pub(crate) fn new(
		pipeline: &Pipeline<P>,
		init_checkpoint: &Checkpoint<P>,
	) -> Self {
		let current = RefCell::new(StepPath::empty());
		let root = Scope::rooted_at(pipeline, init_checkpoint);
		let root = RwLock::new(root);

		Self { root, current }
	}

	/// Given a path to a step in the pipeline, returns its current limits.
	pub(crate) fn limits_of(&self, step_path: &StepPath) -> Option<Limits> {
		self
			.root
			.read()
			.get(&scope_of(step_path))
			.map(|s| *s.limits())
	}

	/// Returns the instant when the scope was last entered.
	pub(crate) fn entered_at(&self, step_path: &StepPath) -> Option<Instant> {
		self
			.root
			.read()
			.get(&scope_of(step_path))
			.and_then(|s| s.started_at())
	}

	/// Called from the pipeline executor when switching between steps.
	/// It detects if the next step is in a different scope and enters and leaves
	/// scopes accordingly. This will leave and enter all intermediate scopes
	/// between the previous and next steps.
	pub(crate) fn switch_context(
		&self,
		next_step: &StepPath,
		checkpoint: &Checkpoint<P>,
	) {
		let next = scope_of(next_step);
		let prev = self.current.replace(next.clone());

		if prev != next {
			// Scope changed. We will need to leave all scopes from `prev`
			// up to the common ancestor, then enter all scopes from the
			// common ancestor to `next`.
			let common = prev.common_ancestor(&next);
			let mut root = self.root.write();

			for s in prev.between(&common) {
				root.get_mut(&s).expect("scope should exist").leave();
			}

			// as we reenter scopes, we will need to regenerate their limits.
			let mut enclosing_limits =
				*root.get_mut(&common).expect("scope should exist").limits();

			for s in common.between(&next) {
				let scope = root.get_mut(&s).expect("scope should exist");

				scope.enter(checkpoint, &enclosing_limits);
				enclosing_limits = *scope.limits();
			}
		}
	}

	pub(crate) fn enter(&self, checkpoint: &Checkpoint<P>) {
		let mut root = self.root.write();
		let limits = root.limits;
		root.enter(checkpoint, &limits);
	}

	pub(crate) fn leave(&self) {
		let mut root = self.root.write();
		root.leave();
	}

	pub(crate) fn is_active(&self) -> bool {
		self.root.read().is_active()
	}
}

unsafe impl<P: Platform> Send for RootScope<P> {}
unsafe impl<P: Platform> Sync for RootScope<P> {}

/// Given a path to a step in the pipeline, returns a path to the immediate
/// pipeline that contains it.
#[inline]
fn scope_of(step: &StepPath) -> StepPath {
	step.clone().remove_leaf().unwrap_or(StepPath::empty())
}

/// Represents a pipeline execution scope.
///
/// Each pipeline has its scope that may include nested scopes for each nested
/// pipeline. Scopes are used to manage limits and metrics for each pipeline
/// execution. All steps in a pipeline run within the scopes of the pipelines
/// that contain it. When a scope is active, then all its parent scopes are
/// active as well.
pub(crate) struct Scope<P: Platform> {
	limits: Limits,
	metrics: Metrics,
	limits_factory: Option<Arc<dyn ScopedLimits<P>>>,
	entered_at: Option<Instant>,
	enter_counter: u32,
	nested: HashMap<usize, Scope<P>>,
}

// public api
impl<P: Platform> Scope<P> {
	/// When a scope is active it means that one of its steps (or in its nested
	/// scopes) is currently being executed,
	pub(crate) const fn is_active(&self) -> bool {
		self.entered_at.is_some()
	}

	/// Returns the elapsed time since the scope was entered.
	/// This will only return a value if the scope is currently active.
	pub(crate) fn elapsed(&self) -> Option<Duration> {
		self.entered_at.map(|start| start.elapsed())
	}

	/// Returns when the scope was entered most recently.
	pub(crate) fn started_at(&self) -> Option<Instant> {
		self.entered_at
	}

	/// Returns the payload limits for steps running within the current scope.
	pub(crate) const fn limits(&self) -> &Limits {
		&self.limits
	}
}

// private api
impl<P: Platform> Scope<P> {
	/// Initialize the root scope of a pipeline and all its nested scopes. This
	/// should be called on the top-level pipeline.
	fn rooted_at(root: &Pipeline<P>, checkpoint: &Checkpoint<P>) -> Self {
		let block = checkpoint.block();
		let platform_limits = P::DefaultLimits::default().create(block);
		let limits_factory = root.limits().cloned();
		let scope_limits = limits_factory
			.as_ref()
			.map_or(platform_limits, |limits_factory| {
				limits_factory.create(checkpoint, &platform_limits)
			})
			.clamp(&platform_limits);

		let scope_name = &format!("{}_pipeline", root.name());
		let metrics = Metrics::with_scope(scope_name);

		let mut nested = HashMap::new();
		for (ix, step) in root.steps().iter().enumerate() {
			if let StepOrPipeline::Pipeline(_, inner) = step {
				let path = StepPath::step(ix);
				let scope =
					Self::inner(inner, checkpoint, &path, root.name(), &scope_limits);
				nested.insert(path.leaf(), scope);
			}
		}

		Self {
			limits: scope_limits,
			metrics,
			nested,
			limits_factory,
			entered_at: None,
			enter_counter: 0,
		}
	}

	fn enter(&mut self, checkpoint: &Checkpoint<P>, enclosing: &Limits) {
		assert!(!self.is_active(), "Scope is already active");

		// refresh limits of this scope
		self.limits = self
			.limits_factory
			.as_ref()
			.map_or(*enclosing, |limits_factory| {
				limits_factory.create(checkpoint, enclosing)
			})
			.clamp(enclosing);

		self.entered_at = Some(Instant::now());
		self.enter_counter = self.enter_counter.saturating_add(1);
		self.metrics.iter_count_total.increment(1);
	}

	fn leave(&mut self) {
		assert!(self.is_active(), "Scope is not active");
		// leave any active nested scope
		for nested in self.nested.values_mut() {
			if nested.is_active() {
				nested.leave();
			}
		}

		let duration = self
			.elapsed()
			.expect("Scope must be entered before leaving");

		#[expect(clippy::cast_possible_truncation)]
		self
			.metrics
			.exec_duration_total_millis
			.increment(duration.as_millis() as u64);

		self.metrics.exec_duration_histogram.record(duration);
		self.entered_at = None;
	}

	/// Returns the scope with a given path.
	fn get_mut(&mut self, step: &StepPath) -> Option<&mut Scope<P>> {
		if *step == StepPath::empty() {
			return Some(self);
		}

		let next = step.root().leaf();
		let suffix = step.clone().remove_root().unwrap_or(StepPath::empty());
		self.nested.get_mut(&next)?.get_mut(&suffix)
	}

	/// Returns the scope with a given path.
	fn get(&self, step: &StepPath) -> Option<&Scope<P>> {
		if *step == StepPath::empty() {
			return Some(self);
		}

		let next = step.root().leaf();
		let suffix = step.clone().remove_root().unwrap_or(StepPath::empty());
		self.nested.get(&next)?.get(&suffix)
	}

	/// Internally construct nested scopes. Called from `rooted_at`.
	fn inner(
		local: &Pipeline<P>,
		checkpoint: &Checkpoint<P>,
		path: &StepPath,
		root_name: &str,
		enclosing: &Limits,
	) -> Self {
		let limits_factory = local.limits().cloned();
		let scope_limits = limits_factory
			.as_ref()
			.map_or(*enclosing, |limits_factory| {
				limits_factory.create(checkpoint, enclosing)
			})
			.clamp(enclosing);

		let scope_name = format!("{root_name}_pipeline_{path}");
		let metrics = Metrics::with_scope(&scope_name);

		let mut nested = HashMap::new();
		for (ix, step) in local.steps().iter().enumerate() {
			if let StepOrPipeline::Pipeline(_, inner) = step {
				let path = path.clone().concat(StepPath::step(ix));
				let scope =
					Self::inner(inner, checkpoint, &path, root_name, &scope_limits);
				nested.insert(path.leaf(), scope);
			}
		}

		Self {
			metrics,
			nested,
			limits_factory,
			limits: scope_limits,
			entered_at: None,
			enter_counter: 0,
		}
	}
}

impl<P: Platform> Drop for Scope<P> {
	fn drop(&mut self) {
		if self.is_active() {
			self.leave();
		}

		self.metrics.iter_count_histogram.record(self.enter_counter);
	}
}

unsafe impl<P: Platform> Send for Scope<P> {}
unsafe impl<P: Platform> Sync for Scope<P> {}

#[derive(MetricsSet)]
pub(crate) struct Metrics {
	/// Histogram of the number of iterations.
	pub iter_count_histogram: Histogram,

	/// Total number of iterations across all payload jobs.
	pub iter_count_total: Counter,

	/// Histogram of the execution duration.
	pub exec_duration_histogram: Histogram,

	/// Total execution duration across all payload jobs.
	pub exec_duration_total_millis: Counter,
}
