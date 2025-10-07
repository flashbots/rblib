use crate::{
	alloy::{
		consensus::transaction::{Transaction, TxHashRef},
		primitives::TxHash,
	},
	prelude::*,
	reth::primitives::Recovered,
};

/// Quality of Life extensions for the `Span` type.
pub trait SpanExt<P: Platform>: super::sealed::Sealed {
	/// Returns the total gas used by all checkpoints in the span.
	fn gas_used(&self) -> u64;

	/// Returns the total blob gas used by all blob transactions in the span.
	fn blob_gas_used(&self) -> u64;

	/// Checks if this span contains a checkpoint with a transaction with a given
	/// hash.
	fn contains(&self, txhash: impl Into<TxHash>) -> bool;

	/// Iterates of all transactions in the span in chronological order as they
	/// appear in the payload under construction.
	///
	/// This iterator returns a reference to each transaction.
	fn transactions(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>>;

	/// Iterates over all blob transactions in the span.
	fn blobs(&self) -> impl Iterator<Item = &Recovered<types::Transaction<P>>>;

	/// Divides the span into two spans at a given index.
	///
	/// The first span will contain all checkpoints from [start, mid),
	/// and the second span will contain all checkpoints from [mid, end].
	///
	/// If `mid` is greater than the length of the span, then the whole span
	/// will be returned as the first span and an empty span will be returned as
	/// the second span.
	fn split_at(&self, mid: usize) -> (Span<P>, Span<P>);

	/// Returns a span that skips the first `n` checkpoints in the span.
	fn skip(&self, n: usize) -> Span<P> {
		let (_, right) = self.split_at(n);
		right
	}

	/// Returns a span that takes the first `n` checkpoints in the span.
	fn take(&self, n: usize) -> Span<P> {
		let (left, _) = self.split_at(n);
		left
	}
}

impl<P: Platform> SpanExt<P> for Span<P> {
	/// Checks if this span contains a checkpoint with a transaction with a given
	/// hash.
	fn contains(&self, txhash: impl Into<TxHash>) -> bool {
		let hash = txhash.into();
		self.iter().any(|checkpoint| {
			checkpoint
				.transactions()
				.iter()
				.any(|tx| *tx.tx_hash() == hash)
		})
	}

	/// Iterates of all transactions in the span in chronological order as they
	/// appear in the payload under construction.
	///
	/// This iterator returns a reference to each transaction.
	fn transactions(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self.iter().flat_map(|checkpoint| checkpoint.transactions())
	}

	/// Iterates over all blob transactions in the span.
	fn blobs(&self) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self
			.transactions()
			.filter(|tx| tx.blob_gas_used().is_some())
	}

	/// Returns the total gas used by all checkpoints in the span.
	fn gas_used(&self) -> u64 {
		self.iter().map(CheckpointExt::gas_used).sum()
	}

	/// Returns the total blob gas used by all blob transactions in the span.
	fn blob_gas_used(&self) -> u64 {
		self
			.iter()
			.flat_map(|checkpoint| {
				checkpoint
					.transactions()
					.iter()
					.filter_map(|tx| tx.blob_gas_used())
			})
			.sum()
	}

	/// Divides the span into two spans at a given index.
	///
	/// The first span will contain all checkpoints from [start, mid),
	/// and the second span will contain all checkpoints from [mid, end].
	///
	/// If `mid` is greater than the length of the span, then the whole span
	/// will be returned as the first span and an empty span will be returned as
	/// the second span.
	fn split_at(&self, mid: usize) -> (Span<P>, Span<P>) {
		let left = self.iter().take(mid).cloned();
		let right = self.iter().skip(mid).cloned();

		// SAFETY: we know that the checkpoints in `left` and `right` form a linear
		// history because they are taken from the same span and spans have no
		// public apis that allow creating non-linear histories.
		unsafe {
			(
				Span::from_iter_unchecked(left),
				Span::from_iter_unchecked(right),
			)
		}
	}
}
