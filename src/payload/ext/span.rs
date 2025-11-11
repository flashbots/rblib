use crate::{
	alloy::primitives::TxHash,
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
	fn contains(&self, tx_hash: impl Into<TxHash>) -> bool;

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
	/// Returns the total gas used by all checkpoints in the span.
	fn gas_used(&self) -> u64 {
		self.iter().map(CheckpointExt::gas_used).sum()
	}

	/// Returns the total blob gas used by all blob transactions in the span.
	fn blob_gas_used(&self) -> u64 {
		self.iter().filter_map(CheckpointExt::blob_gas_used).sum()
	}

	/// Checks if this span contains a checkpoint with a transaction with a given
	/// hash.
	fn contains(&self, tx_hash: impl Into<TxHash>) -> bool {
		let hash = tx_hash.into();
		self.iter().any(|checkpoint| checkpoint.contains(hash))
	}

	/// Iterates of all transactions in the span in chronological order as they
	/// appear in the payload under construction.
	///
	/// This iterator returns a reference to each transaction.
	fn transactions(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self.iter().flat_map(Checkpoint::transactions)
	}

	/// Iterates over all blob transactions in the span.
	fn blobs(&self) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self.iter().flat_map(Checkpoint::blobs)
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

#[cfg(test)]
mod span_ext_tests {
	use {
		crate::{
			payload::{Span, SpanExt},
			prelude::{BlockContext, Checkpoint, Ethereum},
			test_utils::{BlockContextMocked, transfer_tx},
		},
		alloy::primitives::U256,
		rblib::{alloy, test_utils::FundedAccounts},
	};

	#[test]
	fn gas_and_blob_gas_used_sum_correctly() {
		let block = BlockContext::<Ethereum>::mocked();

		let root = block.start();
		let c1: Checkpoint<Ethereum> = root.barrier();
		let c2: Checkpoint<Ethereum> = c1.barrier();

		let span = Span::<Ethereum>::between(&root, &c2).unwrap();

		assert_eq!(span.gas_used(), 0);
		assert_eq!(span.blob_gas_used(), 0);
	}

	#[test]
	fn contains_transaction_hash_works() {
		let block = BlockContext::<Ethereum>::mocked();

		let root = block.start();
		let c1: Checkpoint<Ethereum> = root.barrier();

		let tx1 = transfer_tx::<Ethereum>(
			&FundedAccounts::signer(0),
			0,
			U256::from(50_000u64),
		);

		let c2 = c1.apply(tx1.clone()).unwrap();

		let span = Span::<Ethereum>::between(&root, &c2).unwrap();

		assert!(span.contains(*tx1.hash()));
	}

	#[test]
	fn split_at_produces_valid_halves() {
		let block = BlockContext::<Ethereum>::mocked();

		let root = block.start();
		let c1: Checkpoint<Ethereum> = root.barrier();
		let c2: Checkpoint<Ethereum> = c1.barrier();

		let span = Span::<Ethereum>::between(&root, &c2).unwrap();

		let (left, right) = span.split_at(1);
		assert_eq!(left.len(), 1);
		assert_eq!(right.len(), 2);

		let (left_all, right_empty) = span.split_at(10);
		assert_eq!(left_all.len(), span.len());
		assert!(right_empty.is_empty());
	}

	#[test]
	fn take_and_skip_are_consistent_with_split_at() {
		let block = BlockContext::<Ethereum>::mocked();

		let root = block.start();
		let c1: Checkpoint<Ethereum> = root.barrier();
		let c2: Checkpoint<Ethereum> = c1.barrier();

		let span = Span::<Ethereum>::between(&root, &c2).unwrap();

		let take_1 = span.take(1);
		let skip_1 = span.skip(1);

		assert_eq!(take_1.len(), 1);
		assert_eq!(skip_1.len(), 2);

		let (left, right) = span.split_at(1);
		assert_eq!(take_1.len(), left.len());
		assert_eq!(skip_1.len(), right.len());
	}
}
