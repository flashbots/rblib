//! Pipeline navigation
//!
//! Types in this module are responsible for navigating through the pipeline
//! steps, They identify next steps, manage loops, and handle nested pipelines,
//! etc.

use {
	super::StepInstance,
	crate::prelude::*,
	core::fmt::Formatter,
	derive_more::{From, Into},
	smallvec::{SmallVec, smallvec},
	std::sync::Arc,
};

/// Represents a path to a step or a nested pipeline in a pipeline.
///
/// This type is used to store the current position in the pipeline execution
/// and knows how to navigate through the pipeline structure depending on the
/// current step output and the pipeline structure.
///
/// A Step path cannot be empty; it must always contain at least one element,
/// which is the case for pipelines with only steps and no nested pipelines.
#[derive(PartialEq, Eq, Clone, From, Into, Hash)]
pub(crate) struct StepPath(SmallVec<[usize; 8]>);

const PROLOGUE_INDEX: usize = usize::MIN;
const EPILOGUE_INDEX: usize = usize::MAX;
const STEP0_INDEX: usize = PROLOGUE_INDEX + 1;

/// Public API
impl StepPath {
	/// Creates a new step navigator that binds a step-path to a pipeline.
	/// If the step path points at a nested pipeline, this method will create a
	/// navigator that points to the first executable step starting from the
	/// nested pipeline.
	pub(crate) fn navigator<'a, P: Platform>(
		&self,
		root: &'a Pipeline<P>,
	) -> Option<StepNavigator<'a, P>> {
		let mut ancestors = Vec::with_capacity(self.depth());
		ancestors.push(root);

		let mut last_root = self.root();
		let mut current_path = self.clone();

		while let Some(tail) = current_path.remove_root() {
			let enclosing_pipeline = ancestors.last()?;
			let step_index = last_root.leaf().checked_sub(STEP0_INDEX)?;

			let StepOrPipeline::Pipeline(_, pipeline) =
				enclosing_pipeline.steps().get(step_index)?
			else {
				// invalid step path for the given pipeline
				return None;
			};

			ancestors.push(pipeline);
			last_root = tail.root();
			current_path = tail;
		}

		StepNavigator(self.clone(), ancestors).enter()
	}

	/// Returns the number of nesting levels in the path.
	///
	/// When this path points to an item, this value is the number of pipelines
	/// that contain the item starting from the top-level pipeline.
	pub(crate) fn depth(&self) -> usize {
		self.0.len()
	}

	/// Returns `true` if the path points to a step in a top-level pipeline.
	/// This means that this path is inside a pipeline that has no parents.
	///
	/// In other words, it checks if the path is a single element path.
	pub(crate) fn is_toplevel(&self) -> bool {
		self.depth() == 1
	}

	/// Returns `true` if the path is pointing to a prologue of a pipeline.
	pub(crate) fn is_prologue(&self) -> bool {
		self.leaf() == PROLOGUE_INDEX
	}

	/// Returns `true` if the path is pointing to an epilogue of a pipeline.
	pub(crate) fn is_epilogue(&self) -> bool {
		self.leaf() == EPILOGUE_INDEX
	}
}

/// Private APIs
impl StepPath {
	/// Returns the highest ancestor of the path.
	///
	/// For example, if the path is `[3, 1, 2]`, this function will return `3`,
	/// which represents the nested pipeline at index `3`. A path of `[1]`
	/// represents the step or nested pipeline at index `1` in the pipeline.
	pub(super) fn root(&self) -> StepPath {
		Self(self.0[..1].into())
	}

	/// Returns the index of the step or nested pipeline pointed to by the
	/// path relative to its immediate parent.
	///
	/// This essentially returns the last element of the path.
	pub(super) fn leaf(&self) -> usize {
		self.0.last().copied().expect("StepPath cannot be empty")
	}

	/// Returns a step path without the current root index.
	///
	/// This is useful when doing recursive navigation down through the pipeline.
	/// Returns None if the path is an item in a top-level pipeline.
	pub(super) fn remove_root(self) -> Option<StepPath> {
		if self.is_toplevel() {
			// If there's only one element, popping it would result in an empty path
			// which is not allowed.
			None
		} else {
			Some(StepPath(self.0[1..].into()))
		}
	}

	/// Returns a step path without the current leaf index.
	///
	/// This is useful when you want to navigate up to the pipeline or step that
	/// contains the current step or pipeline.
	///
	/// Returns None if the path points to a top-level item.
	pub(super) fn remove_leaf(self) -> Option<StepPath> {
		if self.is_toplevel() {
			None
		} else {
			Some(StepPath(self.0[..self.0.len() - 1].into()))
		}
	}

	/// Returns a path that points to the next item in the current scope.
	///
	/// This method does not check if the next item is valid or exists in the
	/// pipeline. It simply increments the last index in the path.
	pub(super) fn increment_leaf(self) -> Self {
		let mut new_path = self.0;
		if let Some(last) = new_path.last_mut() {
			*last += 1;
		}
		Self(new_path)
	}

	/// Returns a path with the leaf replaced with the given `new_leaf`
	pub(super) fn replace_leaf(self, new_leaf: usize) -> Self {
		let mut new_path = self.0;
		*new_path.last_mut().expect("StepPath cannot be empty") = new_leaf;
		Self(new_path)
	}

	pub(super) fn is_ancestor_of(&self, other: &StepPath) -> bool {
		other.0.starts_with(&self.0)
	}

	/// Returns a `StepPath` that is the lowest common ancestor of the two paths.
	/// For paths that do not share a common ancestor, this will return an empty
	/// path, which represents the root scope of the pipeline.
	pub(super) fn common_ancestor(&self, other: &StepPath) -> StepPath {
		let mut common = StepPath::empty();
		let mut self_iter = self.0.iter();
		let mut other_iter = other.0.iter();

		while let (Some(&self_index), Some(&other_index)) =
			(self_iter.next(), other_iter.next())
		{
			if self_index == other_index {
				common.0.push(self_index);
			} else {
				break;
			}
		}

		common
	}

	/// Given two paths, where one is an ancestor of the other, returns the
	/// intermediate paths between them.
	///
	/// If the other path is not an ancestor of the other, an empty vector is
	/// returned.
	///
	/// If the paths are equal, an empty vector is returned.
	///
	/// If `self` is an ancestor of `other`, then this will return all the steps
	/// descending from `self` to `other`.
	///
	/// If `other` is an ancestor of `self`, then this will return all the steps
	/// descending from `other` to `self`.
	pub(super) fn between(&self, other: &StepPath) -> Vec<StepPath> {
		if !self.is_ancestor_of(other) && !other.is_ancestor_of(self) {
			return vec![];
		}

		let ancestor = self.common_ancestor(other);
		let mut paths = Vec::new();

		if ancestor == *self {
			// self is the ancestor, we need to go down to other
			let mut current = self.clone();
			while current != *other {
				current.0.push(other.0[current.0.len()]);
				paths.push(current.clone());
			}
		} else if ancestor == *other {
			// other is the ancestor, we need to go up to self
			let mut current = other.clone();
			while current != *self {
				current.0.push(self.0[current.0.len()]);
				paths.push(current.clone());
			}
			paths.reverse();
		}

		paths
	}
}

/// Manual navigation (internal use only)
///
/// None of those methods do any checks on the validity of the path.
impl StepPath {
	/// Returns a step path that points to the prologue step.
	pub(in crate::pipelines) fn prologue() -> Self {
		Self(smallvec![PROLOGUE_INDEX])
	}

	/// Returns a leaf step path pointing at the epilogue step.
	pub(in crate::pipelines) fn epilogue() -> Self {
		Self(smallvec![EPILOGUE_INDEX])
	}

	/// Returns a new step path that points to the first non-prologue step.
	pub(in crate::pipelines) fn step0() -> Self {
		Self::step(0)
	}

	/// Returns a leaf step path pointing at a step with the given index.
	pub(in crate::pipelines) fn step(step_index: usize) -> Self {
		Self(smallvec![step_index + STEP0_INDEX])
	}

	/// Appends a new path to the current path.
	pub(in crate::pipelines) fn concat(self, other: Self) -> Self {
		let mut new_path = self.0;
		new_path.extend(other.0);
		Self(new_path)
	}

	/// Creates an empty step path. This is an invalid state, and public APIs
	/// should never be able to construct an empty `StepPath`.
	///
	/// It is the caller's responsibility to ensure that the returned empty step
	/// path is never used directly, but rather used as a placeholder for
	/// constructing valid paths or as a prefix during traversal or matching.
	///
	/// Empty step paths are used as a placeholder when traversing a pipeline or
	/// as prefix matches for the root pipeline.
	pub(in crate::pipelines) const fn empty() -> Self {
		Self(SmallVec::new_const())
	}
}

impl core::fmt::Display for StepPath {
	fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
		let mut iter = self.0.iter();

		if let Some(&first) = iter.next() {
			match first {
				PROLOGUE_INDEX => write!(f, "p"),
				EPILOGUE_INDEX => write!(f, "e"),
				index => write!(f, "{index}"),
			}?;

			for &index in iter {
				match index {
					PROLOGUE_INDEX => write!(f, "_p"),
					EPILOGUE_INDEX => write!(f, "_e"),
					index => write!(f, "_{index}"),
				}?;
			}
		}
		Ok(())
	}
}

impl core::fmt::Debug for StepPath {
	fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
		write!(f, "StepPath({self})")
	}
}

/// This type is used to navigate through a pipeline.
/// It keeps track of the current step and the hierarchy of enclosing pipelines.
///
/// All public APIs of this type only allow creating instances that point at a
/// step. Internally, it creates temporary versions of itself that point at
/// pipelines when navigating through the pipeline structure, but those
/// instances should never be available to external users of this type.
#[derive(Clone)]
pub(crate) struct StepNavigator<'a, P: Platform>(
	StepPath,
	Vec<&'a Pipeline<P>>,
);

// Public API
impl<'a, P: Platform> StepNavigator<'a, P> {
	/// Given a pipeline, returns a navigator that points at the first executable
	/// item in the pipeline.
	///
	/// In pipelines with a prologue, this will point to the prologue step.
	/// In pipelines without a prologue, this will point to the first step.
	/// In pipelines with no steps, but with an epilogue, this will point to the
	/// epilogue step.
	///
	/// If the first item in the pipeline is a nested pipeline, this will dig
	/// deeper into the nested pipeline to find the first executable item.
	///
	/// In empty pipelines, this will return None.
	pub(crate) fn entrypoint(pipeline: &'a Pipeline<P>) -> Option<Self> {
		if pipeline.is_empty() {
			return None;
		}

		// pipeline has a prologue, return it.
		if pipeline.prologue().is_some() {
			return Some(Self(StepPath::prologue(), vec![pipeline]));
		}

		// pipeline has no prologue
		if pipeline.steps().is_empty() {
			// If there are no steps, but there is an epilogue, return it.
			if pipeline.epilogue().is_some() {
				return Some(Self(StepPath::epilogue(), vec![pipeline]));
			}

			// this is an empty pipeline, there is nothing executable.
			return None;
		}

		// pipeline has steps, dig into the entrypoint of the first item
		Self(StepPath::step0(), vec![pipeline]).enter()
	}

	/// Returns a reference to the instance of the step that this path is
	/// currently pointing to.
	pub(crate) fn instance(&self) -> &Arc<StepInstance<P>> {
		let step_index = self.0.leaf();
		let enclosing_pipeline = self.pipeline();

		if self.is_prologue() {
			enclosing_pipeline
				.prologue()
				.expect("Step path points to a non-existing prologue")
		} else if self.is_epilogue() {
			enclosing_pipeline
				.epilogue()
				.expect("Step path points to a non-existing epilogue")
		} else {
			let StepOrPipeline::Step(step) = enclosing_pipeline
				.steps()
				.get(step_index - STEP0_INDEX)
				.expect("Step path points to a non-existing step")
			else {
				unreachable!(
					"StepNavigator should not point to a pipeline, only to steps"
				)
			};

			step
		}
	}

	/// Returns a reference to the immediate enclosing pipeline that contains the
	/// current step.
	pub(crate) fn pipeline(&self) -> &'a Pipeline<P> {
		self.1.last().expect(
			"StepNavigator should always have at least one enclosing pipeline",
		)
	}

	/// Returns a reference to the top-level pipeline that contains the
	/// current step.
	pub(crate) fn root_pipeline(&self) -> &'a Pipeline<P> {
		self.1.first().expect(
			"StepNavigator should always have at least one enclosing pipeline",
		)
	}

	/// Advance to the next executable step in the pipeline when the current
	/// step's execution returns `ControlFlow::Ok`.
	///
	/// Returns `None` if there are no more steps to execute in the pipeline.
	pub(crate) fn next_ok(self) -> Option<Self> {
		if self.is_epilogue() {
			// the loop is over.
			return self.next_in_parent();
		}

		if self.is_prologue() {
			// start looping (if possible)
			return self.after_prologue();
		}

		let enclosing_pipeline = self.pipeline();

		// we are in a regular step.
		assert!(
			!enclosing_pipeline.steps().is_empty(),
			"invalid navigator state"
		);

		let position = self
			.0
			.leaf()
			.checked_sub(STEP0_INDEX)
			.expect("invalid step index in the step path");

		let is_last = position + 1 >= enclosing_pipeline.steps().len();

		match (self.behavior(), is_last) {
			(Loop, true) => {
				// we are the last step in a loop pipeline, go to the first step.
				Self(self.0.replace_leaf(STEP0_INDEX), self.1.clone()).enter()
			}
			(Once, true) => {
				// we are at the last step in a non-loop pipeline, this is the end of a
				// single iteration loop.
				self.after_loop()
			}
			(_, false) => {
				// if we are not the last step of the pipeline, just go to the
				// next step.
				Self(self.0.increment_leaf(), self.1.clone()).enter()
			}
		}
	}

	/// Advance to the next executable step in the pipeline when the current
	/// step's execution returns `ControlFlow::Break`.
	///
	/// Returns `None` if there are no more steps to execute in the pipeline.
	pub(crate) fn next_break(self) -> Option<Self> {
		if self.is_epilogue() {
			// the loop is over.
			return self.next_in_parent();
		}

		// don't run any further steps in the current scope
		self.after_loop()
	}
}

/// Private APIs
impl<P: Platform> StepNavigator<'_, P> {
	/// Returns the path to the current step relative to the root pipeline.
	pub(crate) const fn path(&self) -> &StepPath {
		&self.0
	}

	/// Returns the loop behavior of the pipeline containing the current step.
	fn behavior(&self) -> Behavior {
		// top-level pipelines are always `Once`.
		if self.0.is_toplevel() {
			return Once;
		}

		// to identify the behavior of the pipeline that contains the current step
		// we need to look at the grandparent pipeline, which contains the immediate
		// parent pipeline that contains the current step, and check the behavior
		// it is configured with.

		let parent_path = self.0.clone().remove_leaf().expect("non-top-level");
		let grandparent_pipeline =
			self.1.iter().rev().nth(1).expect("non-top-level");

		let parent_index = parent_path
			.leaf()
			.checked_sub(STEP0_INDEX)
			.expect("step ancestors may not be prologues or epilogues");

		let StepOrPipeline::Pipeline(behavior, _) = grandparent_pipeline
			.steps()
			.get(parent_index)
			.expect("the parent pipeline should contain the current step")
		else {
			unreachable!("all ancestors of a step must be pipelines");
		};

		*behavior
	}

	/// Creates a new navigator that points to the first ancestor of the current
	/// item pointed to by the path.
	fn ancestor(self) -> Option<Self> {
		let StepNavigator(path, ancestors) = self;
		let path = path.remove_leaf()?;
		let ancestors = ancestors[0..ancestors.len() - 1].to_vec();
		Some(StepNavigator(path, ancestors))
	}

	/// Creates a new navigator that points to the entrypoint of the element
	/// pointed to by the current path.
	///
	/// If the current path points to a step, this will return itself.
	/// If the current path points to a pipeline, this will return the first
	/// executable step in the pipeline.
	/// Returns None if the current path points to an empty pipeline.
	fn enter(self) -> Option<Self> {
		let StepNavigator(path, ancestors) = self;
		let enclosing_pipeline = ancestors.last().expect(
			"StepNavigator should always have at least one enclosing pipeline",
		);

		if path.is_prologue() || path.is_epilogue() {
			assert!(
				enclosing_pipeline.prologue().is_some()
					|| enclosing_pipeline.epilogue().is_some(),
				"path is prologue or epilogue, but enclosing pipeline has none",
			);
			// if we are in a prologue or epilogue, we can just return ourselves.
			return Some(Self(path, ancestors));
		}

		let step_index = path
			.leaf()
			.checked_sub(STEP0_INDEX)
			.expect("path is not prologue or epilogue");

		match enclosing_pipeline.steps().get(step_index)? {
			StepOrPipeline::Step(_) => {
				// if we are pointing at a step, we can just return ourselves.
				Some(Self(path, ancestors))
			}
			StepOrPipeline::Pipeline(_, nested) => {
				// if we are pointing at a pipeline, we need to dig into its
				// entrypoint.
				Some(StepNavigator(path, ancestors).join(Self::entrypoint(nested)?))
			}
		}
	}

	/// Finds the next step to run when a loop is finished.
	///
	/// The next step could be either the epilogue step of the current
	/// pipeline or the next step in the parent pipeline.
	fn after_loop(self) -> Option<Self> {
		if self.pipeline().epilogue().is_some() {
			// we've reached the epilogue of this pipeline, regardless of the
			// looping behavior, we should go to the next step in the parent pipeline.
			Some(Self(self.0.replace_leaf(EPILOGUE_INDEX), self.1.clone()))
		} else {
			self.next_in_parent()
		}
	}

	/// Finds the next step to run after the prologue of the current pipeline.
	fn after_prologue(self) -> Option<Self> {
		if self.pipeline().steps().is_empty() {
			// no steps, go to epilogue.
			self.after_loop()
		} else {
			// this pipeline has steps. Go to the first step entrypoint
			Self(self.0.replace_leaf(STEP0_INDEX), self.1.clone()).enter()
		}
	}

	/// Runs the next step in the parent pipeline.
	fn next_in_parent(self) -> Option<Self> {
		// we're guaranteed that the ancestor is not an epilogue or prologue,
		let ancestor = self.ancestor()?;
		let step_index = ancestor.0.leaf().checked_sub(STEP0_INDEX)?;
		let enclosing_pipeline = ancestor.1.last().expect(
			"StepNavigator should always have at least one enclosing pipeline",
		);

		// is the last step in the enclosing pipeline?
		if step_index + 1 >= enclosing_pipeline.steps().len() {
			match ancestor.behavior() {
				Loop => ancestor.after_prologue(),
				Once => ancestor.after_loop(),
			}
		} else {
			// there are more items in the enclosing pipeline, so we can just
			// increment the step index and return the new navigator to the first
			// executable step in the next item.
			Self(ancestor.0.increment_leaf(), ancestor.1.clone()).enter()
		}
	}

	fn join(self, other: Self) -> Self {
		Self(self.0.concat(other.0), [self.1, other.1].concat())
	}

	fn is_epilogue(&self) -> bool {
		self.0.is_epilogue()
	}

	fn is_prologue(&self) -> bool {
		self.0.is_prologue()
	}
}

impl<'a, P: Platform> From<StepNavigator<'a, P>> for StepPath {
	/// Converts a step navigator to a step path.
	///
	/// This is useful when you want to get the path of the current step in the
	/// pipeline.
	fn from(navigator: StepNavigator<'a, P>) -> Self {
		navigator.0
	}
}

#[cfg(test)]
mod test {
	use {super::*, crate::test_utils::*};

	fake_step!(Epilogue1);
	fake_step!(Epilogue2);
	fake_step!(Epilogue3);

	fake_step!(Prologue1);
	fake_step!(Prologue2);

	fake_step!(Step1);
	fake_step!(Step2);
	fake_step!(Step3);
	fake_step!(Step4);

	fake_step!(StepA);
	fake_step!(StepC);

	fake_step!(StepX);
	fake_step!(StepY);
	fake_step!(StepZ);

	fake_step!(StepI);
	fake_step!(StepII);
	fake_step!(StepIII);

	fn top_pipeline() -> Pipeline<Ethereum> {
		Pipeline::<Ethereum>::named("top")
			.with_step(Step1)
			.with_step(Step2)
			.with_pipeline(Loop, |nested: Pipeline<Ethereum>| {
				nested
					.with_name("nested1")
					.with_prologue(Prologue1)
					.with_step(StepA)
					.with_pipeline(
						Loop,
						(StepX, StepY, StepZ)
							.with_name("nested1.1")
							.with_prologue(Prologue2)
							.with_epilogue(Epilogue2),
					)
					.with_step(StepC)
					.with_pipeline(
						Once,
						(StepI, StepII, StepIII)
							.with_name("nested1.2")
							.with_epilogue(Epilogue3),
					)
			})
			.with_step(Step4)
			.with_epilogue(Epilogue1)
	}

	impl StepPath {
		fn append_prologue(self) -> Self {
			self.concat(StepPath::prologue())
		}

		fn append_epilogue(self) -> Self {
			self.concat(StepPath::epilogue())
		}

		fn append_step(self, step_index: usize) -> Self {
			self.concat(StepPath::step(step_index))
		}
	}

	#[test]
	fn find_entrypoint() {
		let empty = Pipeline::<Ethereum>::default();
		assert!(StepNavigator::entrypoint(&empty).is_none());

		macro_rules! assert_entrypoint {
			($pipeline:expr, $expected_path:expr, $expected_named:expr) => {{
				let pipeline = $pipeline;
				let StepNavigator(path, pipelines) =
					StepNavigator::entrypoint(&pipeline).unwrap();
				assert_eq!(path, $expected_path);
				assert_eq!(
					pipelines.iter().map(|p| p.name()).collect::<Vec<&str>>(),
					$expected_named
				);
			}};
		}

		// one step with no prologue
		assert_entrypoint!(
			Pipeline::<Ethereum>::default().with_step(Step1),
			StepPath::step0(),
			// name autogenerated from the source location
			vec![format!("navi_{}", line!() - 3)]
		);

		// one step with prologue
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one")
				.with_prologue(Prologue1)
				.with_step(Step1),
			StepPath::prologue(),
			vec!["one"]
		);

		// no steps, but with epilogue
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one").with_epilogue(Epilogue1),
			StepPath::epilogue(),
			vec!["one"]
		);

		// one nested step with no prologue
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one")
				.with_pipeline(Loop, (Step1,).with_name("two")),
			StepPath::step0().append_step(0),
			vec!["one", "two"]
		);

		// one nested step with prologue
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one").with_pipeline(
				Loop,
				(Step1,).with_prologue(Prologue1).with_name("two")
			),
			StepPath::step0().append_prologue(),
			vec!["one", "two"]
		);

		// one nested pipeline with no steps, but with an epilogue
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one")
				.with_pipeline(Loop, Pipeline::named("two").with_epilogue(Epilogue1)),
			StepPath::step0().append_epilogue(),
			vec!["one", "two"]
		);

		let nested_empty_pipeline =
			Pipeline::<Ethereum>::default().with_pipeline(Loop, Pipeline::default());
		assert!(StepNavigator::entrypoint(&nested_empty_pipeline).is_none());

		// two levels of nested steps with no prologue
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one").with_pipeline(
				Loop,
				Pipeline::named("two").with_pipeline(Loop, (Step1,).with_name("three"))
			),
			StepPath::step(0).append_step(0).append_step(0),
			vec!["one", "two", "three"]
		);

		// two levels of nested steps with prologue at the first level
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one").with_pipeline(
				Loop,
				Pipeline::named("two")
					.with_prologue(Prologue1)
					.with_pipeline(Loop, (Step1,).with_name("three"))
			),
			StepPath::step(0).append_prologue(),
			vec!["one", "two"]
		);

		// two levels of nested steps with prologue at the second level
		assert_entrypoint!(
			Pipeline::<Ethereum>::named("one").with_pipeline(
				Loop,
				Pipeline::named("two").with_pipeline(
					Loop,
					(Step1,).with_prologue(Prologue1).with_name("three")
				)
			),
			StepPath::step(0).append_step(0).append_prologue(),
			vec!["one", "two", "three"]
		);
	}

	#[test]
	fn identify_loop_behavior() {
		let pipeline = Pipeline::<Ethereum>::default()
			.with_step(Step1)
			.with_step(Step2)
			.with_step(Step3);
		let navigator = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(navigator.behavior(), Once);

		let pipeline =
			Pipeline::<Ethereum>::default().with_pipeline(Loop, (Step1,));
		let navigator = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(navigator.behavior(), Loop);

		let pipeline =
			Pipeline::<Ethereum>::default().with_pipeline(Once, (Step1,));
		let navigator = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(navigator.behavior(), Once);
	}

	#[test]
	fn step_ref_access() {
		let pipeline = Pipeline::<Ethereum>::default()
			.with_step(Step1)
			.with_step(Step2)
			.with_step(Step3);
		let navigator = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(navigator.instance().name(), "Step1");

		let pipeline = Pipeline::<Ethereum>::default()
			.with_prologue(Prologue1)
			.with_step(Step1)
			.with_step(Step2)
			.with_step(Step3);
		let navigator = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(navigator.instance().name(), "Prologue1");

		let pipeline = Pipeline::<Ethereum>::default()
			.with_pipeline(Loop, Pipeline::default().with_epilogue(Epilogue1));
		let navigator = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(navigator.instance().name(), "Epilogue1");
	}

	#[test]
	fn control_flow() {
		let pipeline = top_pipeline();

		let cursor = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(cursor.0, StepPath::step0());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(1));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_prologue());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(0));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_prologue());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_step(0));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_step(1));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_step(2));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_step(0));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_step(1));

		let cursor = cursor.next_break().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_epilogue());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(2));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(3).append_step(0));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(3).append_step(1));

		let cursor = cursor.next_break().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(3).append_epilogue());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(0));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_prologue());

		let cursor = cursor.next_break().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_epilogue());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(2));

		let cursor = cursor.next_break().unwrap();
		assert_eq!(cursor.0, StepPath::step(3));

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::epilogue());

		let cursor = cursor.next_ok();
		assert!(cursor.is_none());
	}

	#[test]
	fn create_navigator() {
		let pipeline = top_pipeline();

		let cursor = StepNavigator::entrypoint(&pipeline).unwrap();
		assert_eq!(cursor.0, StepPath::step0());
		let navigator = cursor.0.navigator(&pipeline).unwrap();
		assert_eq!(navigator.0, StepPath::step0());
		assert_eq!(navigator.instance().name(), cursor.instance().name());
		assert_eq!(navigator.1.len(), cursor.1.len());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(1));
		let navigator = cursor.0.navigator(&pipeline).unwrap();
		assert_eq!(navigator.0, StepPath::step(1));
		assert_eq!(navigator.1.len(), cursor.1.len());
		assert_eq!(navigator.instance().name(), cursor.instance().name());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_prologue());
		let navigator = cursor.0.navigator(&pipeline).unwrap();
		assert_eq!(navigator.1.len(), cursor.1.len());
		assert_eq!(navigator.0, StepPath::step(2).append_prologue());
		assert_eq!(navigator.instance().name(), cursor.instance().name());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(0));
		let navigator = cursor.0.navigator(&pipeline).unwrap();
		assert_eq!(navigator.1.len(), cursor.1.len());
		assert_eq!(navigator.0, StepPath::step(2).append_step(0));
		assert_eq!(navigator.instance().name(), cursor.instance().name());

		let cursor = cursor.next_ok().unwrap();
		assert_eq!(cursor.0, StepPath::step(2).append_step(1).append_prologue());
		let navigator = cursor.0.navigator(&pipeline).unwrap();
		assert_eq!(navigator.1.len(), cursor.1.len());
		assert_eq!(
			navigator.0,
			StepPath::step(2).append_step(1).append_prologue()
		);
		assert_eq!(navigator.instance().name(), cursor.instance().name());

		// navigator goes to the first executable step rooted at the path
		let navigator = StepPath::step(2).navigator(&pipeline).unwrap();
		assert_eq!(navigator.0, StepPath::step(2).append_prologue());
		assert_eq!(navigator.instance().name(), "Prologue1");
	}

	#[test]
	fn is_ancestor_of() {
		assert!(
			StepPath::step(1)
				.append_step(2)
				.append_step(3)
				.is_ancestor_of(
					&StepPath::step(1)
						.append_step(2)
						.append_step(3)
						.append_step(4)
				)
		);

		assert!(
			!StepPath::step(2)
				.append_step(2)
				.append_step(3)
				.is_ancestor_of(
					&StepPath::step(1)
						.append_step(2)
						.append_step(3)
						.append_step(4)
				)
		);
	}

	#[test]
	fn common_ancestor() {
		assert_eq!(
			StepPath::step(1)
				.append_step(2)
				.append_step(3)
				.common_ancestor(
					&StepPath::step(1)
						.append_step(2)
						.append_step(3)
						.append_step(4)
				),
			StepPath::step(1).append_step(2).append_step(3)
		);

		assert_eq!(
			StepPath::step(1)
				.append_step(2)
				.append_step(3)
				.common_ancestor(
					&StepPath::step(2)
						.append_step(2)
						.append_step(3)
						.append_step(4)
				),
			StepPath::empty()
		);

		assert_eq!(
			StepPath::step(1)
				.append_step(2)
				.common_ancestor(&StepPath::step(1).append_step(2).append_step(3)),
			StepPath::step(1).append_step(2)
		);

		assert_eq!(
			StepPath::step(1).common_ancestor(&StepPath::step(1).append_step(2)),
			StepPath::step(1)
		);

		assert_eq!(
			StepPath::empty().common_ancestor(&StepPath::empty()),
			StepPath::empty()
		);
	}

	#[test]
	fn between() {
		let p1 = StepPath::step(1).append_step(2);
		let p2 = StepPath::step(1)
			.append_step(2)
			.append_step(3)
			.append_step(4);

		assert_eq!(p1.between(&p2), vec![
			StepPath::step(1).append_step(2).append_step(3),
			StepPath::step(1)
				.append_step(2)
				.append_step(3)
				.append_step(4)
		]);

		assert_eq!(p2.between(&p1), vec![
			StepPath::step(1)
				.append_step(2)
				.append_step(3)
				.append_step(4),
			StepPath::step(1).append_step(2).append_step(3),
		]);

		assert_eq!(p1.between(&StepPath::empty()), vec![
			StepPath::step(1).append_step(2),
			StepPath::step(1)
		]);

		assert_eq!(p2.between(&StepPath::empty()), vec![
			StepPath::step(1)
				.append_step(2)
				.append_step(3)
				.append_step(4),
			StepPath::step(1).append_step(2).append_step(3),
			StepPath::step(1).append_step(2),
			StepPath::step(1)
		]);

		assert_eq!(StepPath::empty().between(&p2), vec![
			StepPath::step(1),
			StepPath::step(1).append_step(2),
			StepPath::step(1).append_step(2).append_step(3),
			StepPath::step(1)
				.append_step(2)
				.append_step(3)
				.append_step(4),
		]);

		// no steps between
		let p1 = StepPath::step(2).append_step(2);
		let p2 = StepPath::step(1)
			.append_step(2)
			.append_step(3)
			.append_step(4);

		assert_eq!(p1.between(&p2), vec![]);
		assert_eq!(p2.between(&p1), vec![]);
	}
}
