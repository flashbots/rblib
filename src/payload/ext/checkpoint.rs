use {
	crate::{
		alloy::{
			consensus::Transaction,
			primitives::{Address, B256, U256},
		},
		prelude::*,
		reth::{
			errors::ProviderError,
			ethereum::primitives::SignedTransaction,
			primitives::Recovered,
			revm::DatabaseRef,
		},
	},
	itertools::Itertools,
	std::time::Instant,
};

/// Quality of Life extensions for the `Checkpoint` type.
pub trait CheckpointExt<P: Platform>: super::sealed::Sealed {
	/// Returns `true` if this checkpoint is the baseline checkpoint in the
	/// history, and has no transactions in its history.
	fn is_empty(&self) -> bool;

	/// Returns the first checkpoint in the chain of checkpoints since the
	/// beginning of the block payload we're building.
	#[must_use]
	fn root(&self) -> Self;

	/// Gas used by this checkpoint.
	fn gas_used(&self) -> u64;

	/// Returns the cumulative gas used by all checkpoints in the history of this
	/// checkpoint, including this checkpoint itself.
	fn cumulative_gas_used(&self) -> u64 {
		self.history().gas_used()
	}

	/// Returns the cumulative blob gas used by all checkpoints in the history of
	/// this checkpoint, including this checkpoint itself.
	fn cumulative_blob_gas_used(&self) -> u64 {
		self.history().blob_gas_used()
	}

	/// Returns the effective tip for this transaction.
	fn effective_tip_per_gas(&self) -> u128;

	/// If this checkpoint was created by applying a blob transaction,
	/// returns the blob gas used by the blob transaction, `None` otherwise.
	fn blob_gas_used(&self) -> Option<u64>;

	/// Returns `true` if this checkpoint was created by applying EIP-4844 blob
	/// transaction, `false` otherwise.
	fn has_blobs(&self) -> bool;

	/// Returns a span that includes all checkpoints from the beginning of the
	/// block payload we're building to the current checkpoint.
	fn history(&self) -> Span<P>;

	/// Returns a span that includes all staging history of this checkpoint,
	/// which is all preceding checkpoints from the last barrier, or the entire
	/// history if there is no barrier checkpoint.
	fn history_staging(&self) -> Span<P> {
		let history = self.history();
		let immutable_prefix = history
			.iter()
			.rposition(Checkpoint::is_barrier)
			.unwrap_or(0);
		history.skip(immutable_prefix)
	}

	/// Returns a span that includes all checkpoints in the sealed history,
	/// that is the history from the beginning of the block until the last
	/// barrier included. If there are no barriers, the entire history is
	/// returned.
	fn history_sealed(&self) -> Span<P> {
		let history = self.history();
		let immutable_prefix = history
			.iter()
			.rposition(Checkpoint::is_barrier)
			.unwrap_or(0);
		history.take(immutable_prefix + 1)
	}

	/// Creates a new span that includes this checkpoints and all other
	/// checkpoints that are between this checkpoint and the given checkpoint.
	///
	/// The two checkpoints must be part of the same linear history, meaning that
	/// one of them must be a descendant of the other.
	///
	/// The other checkpoint can be either a previous or a future checkpoint.
	fn to(&self, other: &Self) -> Result<Span<P>, SpanError>;

	/// Returns the balance of a given address at this checkpoint.
	fn balance_of(&self, address: Address) -> Result<U256, ProviderError>;

	/// Returns the nonce of a given account at this checkpoint.
	fn nonce_of(&self, address: Address) -> Result<u64, ProviderError>;

	/// Returns all nonces for each signer in this checkpoint.
	fn nonces(&self) -> Vec<(Address, u64)>;

	/// Returns a list of all unique signers in this checkpoint.
	fn signers(&self) -> Vec<Address>;

	/// For checkpoints that are not barriers, returns the transaction or bundle
	/// hash.
	fn hash(&self) -> Option<B256>;

	/// Returns `true` if this checkpoint has any transactions with non-success
	/// execution outcome.
	fn has_failures(&self) -> bool;

	/// Returns an iterator over all transactions in this checkpoint that did not
	/// have a successful execution outcome.
	fn failed_txs(
		&self,
	) -> impl Iterator<
		Item = (
			&Recovered<types::Transaction<P>>,
			&types::TransactionExecutionResult<P>,
		),
	>;

	/// Returns true if the checkpoint represents a bundle of transactions.
	fn is_bundle(&self) -> bool;

	/// Returns the timestamp of initial checkpoint that was created at the very
	/// beginning of the payload building process. It is the timestamp if the
	/// first checkpoint in the history of this checkpoint that is created
	/// by `Checkpoint::new_at_block`.
	fn building_since(&self) -> Instant;
}

impl<P: Platform> CheckpointExt<P> for Checkpoint<P> {
	/// Returns `true` if this checkpoint is the baseline checkpoint in the
	/// history, and has no transactions in its history.
	fn is_empty(&self) -> bool {
		self.depth() == 0
	}

	/// Returns the first checkpoint in the chain of checkpoints since the
	/// beginning of the block payload we're building.
	fn root(&self) -> Checkpoint<P> {
		let mut current = self.clone();
		while let Some(prev) = current.prev() {
			current = prev;
		}
		current
	}

	/// Gas used by this checkpoint.
	fn gas_used(&self) -> u64 {
		self.result().map_or(0, |result| result.gas_used())
	}

	/// Returns the sum of effective tips for transactions in this checkpoint.
	fn effective_tip_per_gas(&self) -> u128 {
		self
			.transactions()
			.iter()
			.filter_map(|tx| tx.effective_tip_per_gas(self.block().base_fee()))
			.sum()
	}

	/// If this checkpoint has EIP-4844 blob transactions,
	/// returns the sum of all blob gas used, `None` otherwise.
	fn blob_gas_used(&self) -> Option<u64> {
		self
			.transactions()
			.iter()
			.map(|tx| tx.blob_gas_used())
			.sum()
	}

	/// Returns `true` if this checkpoint was created by applying an EIP-4844 blob
	/// transaction, `false` otherwise.
	fn has_blobs(&self) -> bool {
		self.blob_gas_used().is_some()
	}

	/// Returns a span that includes all checkpoints from the beginning of the
	/// block payload we're building to the current checkpoint.
	///
	/// This span is guaranteed to always have at least one checkpoint,
	/// which is the baseline checkpoint that is the root of the
	/// checkpoint history.
	fn history(&self) -> Span<P> {
		Span::between(self, &self.root())
			.expect("history is always linear between self and root")
	}

	/// Creates a new span that includes this checkpoints and all other
	/// checkpoints that are between this checkpoint and the given checkpoint.
	///
	/// The two checkpoints must be part of the same linear history, meaning that
	/// one of them must be a descendant of the other.
	///
	/// The other checkpoint can be either a previous or a future checkpoint.
	fn to(&self, other: &Checkpoint<P>) -> Result<Span<P>, SpanError> {
		Span::between(self, other)
	}

	/// Returns the balance of a given address at this checkpoint.
	fn balance_of(&self, address: Address) -> Result<U256, ProviderError> {
		Ok(
			self
				.basic_ref(address)?
				.map(|basic| basic.balance)
				.unwrap_or_default(),
		)
	}

	/// Returns the nonce of a given account at this checkpoint.
	fn nonce_of(&self, address: Address) -> Result<u64, ProviderError> {
		Ok(
			self
				.basic_ref(address)?
				.map(|basic| basic.nonce)
				.unwrap_or_default(),
		)
	}

	/// Returns all nonces for each signer in this checkpoint.
	fn nonces(&self) -> Vec<(Address, u64)> {
		self
			.transactions()
			.iter()
			.map(|tx| (tx.signer(), tx.nonce()))
			.collect()
	}

	/// Returns a list of all signers in this checkpoint.
	fn signers(&self) -> Vec<Address> {
		self
			.transactions()
			.iter()
			.map(|tx| tx.signer())
			.unique()
			.collect()
	}

	/// For checkpoints that are not barriers, returns the transaction or bundle
	/// hash.
	fn hash(&self) -> Option<B256> {
		if let Some(tx) = self.as_transaction() {
			Some(*tx.tx_hash())
		} else {
			self.as_bundle().map(|bundle| bundle.hash())
		}
	}

	/// Returns `true` if this checkpoint has any transactions with non-success
	/// execution outcome.
	fn has_failures(&self) -> bool {
		self.failed_txs().next().is_some()
	}

	/// Returns an iterator over all transactions in this checkpoint that did not
	/// have a successful execution outcome.
	fn failed_txs(
		&self,
	) -> impl Iterator<
		Item = (
			&Recovered<types::Transaction<P>>,
			&types::TransactionExecutionResult<P>,
		),
	> {
		self.result().into_iter().flat_map(|result| {
			result
				.transactions()
				.iter()
				.zip(result.results())
				.filter_map(|(tx, res)| (!res.is_success()).then_some((tx, res)))
		})
	}

	/// Returns true if the checkpoint represents a bundle of transactions.
	fn is_bundle(&self) -> bool {
		self.as_bundle().is_some()
	}

	/// Returns the timestamp of initial checkpoint that was created at the very
	/// beginning of the payload building process. It is the timestamp if the
	/// first barrier checkpoint in the history of this checkpoint that is created
	/// by `Checkpoint::new_at_block`.
	fn building_since(&self) -> Instant {
		let mut created_at = self.created_at();
		let mut current = self.clone();
		while let Some(prev) = current.prev() {
			created_at = prev.created_at();
			current = prev;
		}
		created_at
	}
}
