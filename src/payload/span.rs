use {
	crate::prelude::*,
	core::fmt::Display,
	std::collections::{VecDeque, vec_deque::IntoIter},
	thiserror::Error,
};

/// A span represents a sequence of checkpoints in the payload building process
/// with linear history. While checkpoints only allow traversing the history
/// backwards, a span allows traversing the history forwards as well.
///
/// Using a span, you can also treat a number of checkpoints as a single
/// aggregate of state transitions.
///
/// A span can be used as a database reference for an evm instance. When a span
/// is used as a database, only new states that were created or modified by
/// checkpoints in the span are visible. Any states from checkpoints outside
/// the span or the base state of the block are not available.
#[derive(Clone)]
pub struct Span<P: Platform> {
	checkpoints: VecDeque<Checkpoint<P>>,
}

#[derive(Debug, Clone, Error)]
pub enum Error {
	/// This error means that one of the checkpoints is not a descendant of
	/// the other. A span cannot be created as the history between the checkpoints
	/// is not linear.
	#[error("There is no linear history between the checkpoints")]
	NonlinearHistory,
}

/// Construction
impl<P: Platform> Span<P> {
	/// Attempts to create a span between two checkpoints.
	///
	/// The order of checkpoints does not matter as long as one of them is a
	/// descendant of the other.
	pub fn between(
		start: &Checkpoint<P>,
		end: &Checkpoint<P>,
	) -> Result<Self, Error> {
		let start = start.clone();
		let end = end.clone();

		if start == end {
			// it's a single-checkpoint span,
			// we can return it directly
			return Ok(Self {
				checkpoints: [start.clone()].into_iter().collect(),
			});
		}

		// find out which checkpoint is a descendant of the other
		let (ancestor, descendant) = if start.depth() < end.depth() {
			(start, end)
		} else {
			(end, start)
		};

		// Gather all checkpoints from the descendant to the ancestor
		// and ensure that the history is linear.

		let distance = descendant.depth().saturating_sub(ancestor.depth());
		let mut checkpoints = Vec::with_capacity(distance.saturating_add(1));

		let mut current = descendant;
		checkpoints.push(current.clone());

		while let Some(prev) = current.prev() {
			if prev.depth() < ancestor.depth() {
				return Err(Error::NonlinearHistory);
			}

			checkpoints.push(prev.clone());

			if prev == ancestor {
				// we've reached the ancestor checkpoint, and we have linear history.
				// The collected checkpoints are in reverse order, so we need to
				// reverse them before returning.
				return Ok(Self {
					checkpoints: checkpoints.into_iter().rev().collect(),
				});
			}

			current = prev;
		}

		Err(Error::NonlinearHistory)
	}
}

/// Public APIs
impl<P: Platform> Span<P> {
	/// The number of checkpoints in the span.
	pub fn len(&self) -> usize {
		self.checkpoints.len()
	}

	/// Returns `true` if the span is empty, `false` otherwise.
	pub fn is_empty(&self) -> bool {
		self.checkpoints.is_empty()
	}

	/// Returns the first checkpoint in the span
	pub fn first(&self) -> Option<&Checkpoint<P>> {
		self.checkpoints.front()
	}

	/// Returns the last checkpoint in the span.
	pub fn last(&self) -> Option<&Checkpoint<P>> {
		self.checkpoints.back()
	}

	/// Removes the first checkpoint from the span and returns it.
	/// Returns `None` if the span is empty.
	pub fn pop_first(&mut self) -> Option<Checkpoint<P>> {
		self.checkpoints.pop_front()
	}

	/// Returns a span with the last checkpoint removed.
	pub fn pop_last(&mut self) -> Option<Checkpoint<P>> {
		self.checkpoints.pop_back()
	}

	/// Returns a checkpoint at the given index relative to the start of the span.
	pub fn at(&self, index: usize) -> Option<&Checkpoint<P>> {
		self.checkpoints.get(index)
	}

	/// Returns an iterator over the checkpoints in the span.
	/// The iteration order is from the ancestor checkpoint to the descendant
	/// checkpoint.
	pub fn iter(
		&self,
	) -> impl DoubleEndedIterator<Item = &Checkpoint<P>> + ExactSizeIterator + Clone
	{
		self.checkpoints.iter()
	}
}

/// Internal APIs
impl<P: Platform> Span<P> {
	/// Creates a span from an iterator of checkpoints.
	///
	/// This is an internal method and does not check if checkpoints
	/// form a linear history. It's the caller's responsibility to ensure
	/// that the checkpoints are in the correct order and form a linear history.
	pub(crate) unsafe fn from_iter_unchecked(
		checkpoints: impl Iterator<Item = Checkpoint<P>>,
	) -> Self {
		Self {
			checkpoints: checkpoints.collect(),
		}
	}
}

impl<P: Platform> IntoIterator for Span<P> {
	type IntoIter = IntoIter<Checkpoint<P>>;
	type Item = Checkpoint<P>;

	fn into_iter(self) -> Self::IntoIter {
		self.checkpoints.into_iter()
	}
}

impl<P: Platform> Display for Span<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		if self.is_empty() {
			return write!(f, "[empty]");
		}

		write!(
			f,
			"[checkpoints {}-{} / {} txs] ({} gas, {} blob gas)",
			&self.first().unwrap().depth(),
			&self.last().unwrap().depth(),
			&self.transactions().count(),
			&self.gas_used(),
			&self.blob_gas_used()
		)
	}
}

#[cfg(test)]
mod tests {
	use crate::{
		payload::Span,
		prelude::{BlockContext, Checkpoint, Ethereum},
		test_utils::BlockContextMocked,
	};

	#[test]
	fn span_between_linear_history() {
		let block = BlockContext::<Ethereum>::mocked();

		let checkpoint = block.start();
		let c1: Checkpoint<Ethereum> = checkpoint.barrier();
		let c2: Checkpoint<Ethereum> = c1.barrier();

		// Direct ancestor-descendant pair
		let mut span = Span::<Ethereum>::between(&checkpoint, &c2).unwrap();
		assert_eq!(span.len(), 3);
		assert_eq!(span.pop_first().unwrap(), checkpoint);
		assert_eq!(span.len(), 2);
		assert_eq!(span.pop_last().unwrap(), c2);
		assert_eq!(span.len(), 1);
	}

	#[test]
	fn test_empty_span() {
		let block = BlockContext::<Ethereum>::mocked();

		let checkpoint = block.start();

		let span = Span::<Ethereum>::between(&checkpoint, &checkpoint).unwrap();

		assert_eq!(span.len(), 1);
	}

	#[test]
	fn test_between_nonlinear_history() {
		let block = BlockContext::<Ethereum>::mocked();

		let checkpoint1 = block.start();
		let checkpoint2 = block.start();

		let result = Span::between(&checkpoint1, &checkpoint2);
		assert!(result.is_err());
	}

	#[test]
	fn into_iter_and_iterators_work() {
		let block = BlockContext::<Ethereum>::mocked();

		let root = block.start();
		let c1: Checkpoint<Ethereum> = root.barrier();
		let c2: Checkpoint<Ethereum> = c1.barrier();

		let span = Span::<Ethereum>::between(&root, &c2).unwrap();

		// iter()
		let ids_from_iter: Vec<_> = span.iter().map(|c| c.to_owned()).collect();
		assert_eq!(ids_from_iter.len(), 3);
		assert_eq!(ids_from_iter[0], root);
		assert_eq!(ids_from_iter[2], c2);

		// into_iter()
		let ids_from_into_iter: Vec<_> = span.clone().into_iter().collect();
		assert_eq!(ids_from_iter, ids_from_into_iter);
	}
}
