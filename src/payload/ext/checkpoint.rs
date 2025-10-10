use {
	crate::{
		alloy::{
			consensus::transaction::{Transaction, TxHashRef},
			primitives::{Address, B256, TxHash, U256},
		},
		prelude::*,
		reth::{errors::ProviderError, primitives::Recovered, revm::DatabaseRef},
	},
	itertools::Itertools,
	std::time::Instant,
};

/// Quality of Life extensions for the `Checkpoint` type.
pub trait CheckpointExt<P: Platform>: super::sealed::Sealed {
	/// Returns `true` if this checkpoint is the baseline checkpoint in the
	/// history and has no transactions in its history.
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

	/// Returns the effective tip of the checkpoint, as the cumulative effective
	/// tip of all transactions in this checkpoint.
	fn effective_tip_per_gas(&self) -> u128;

	/// If this checkpoint was created by applying one or more blobs transactions,
	/// returns the cumulative blob gas used by theses, `None` otherwise.
	fn blob_gas_used(&self) -> Option<u64>;

	/// Returns `true` if this checkpoint was created by applying at least one
	/// EIP-4844 blob transaction, `false` otherwise.
	fn has_blobs(&self) -> bool;

	//// Iterates over all blob transactions in the checkpoint.
	fn blobs(&self) -> impl Iterator<Item = &Recovered<types::Transaction<P>>>;

	/// Returns a span that includes all checkpoints from the beginning of the
	/// block payload we're building up to the current one (included).
	fn history(&self) -> Span<P>;

	/// Check if the checkpoint contains a transaction with a given hash.
	fn contains(&self, tx_hash: impl Into<TxHash>) -> bool;

	/// Returns a span that includes the staging history of this checkpoint.
	/// The staging history of a checkpoint is all the preceding checkpoints from
	/// the last barrier up to this current checkpoint, or the entire history if
	/// there is no barrier checkpoint. If the current checkpoint is a barrier
	/// the staging history will be empty.
	fn history_staging(&self) -> Span<P> {
		let history = self.history();
		let immutable_prefix = history
			.iter()
			.rposition(Checkpoint::is_barrier)
			.unwrap_or(0);
		history.skip(immutable_prefix)
	}

	/// Returns a span that includes the sealed history of this checkpoint.
	/// The sealed history of a checkpoint is all checkpoints from the
	/// beginning of the block building job until the last barrier included.
	/// If there are no barriers, the sealed history is empty
	fn history_sealed(&self) -> Span<P> {
		let history = self.history();
		let mutable_start = history
			.iter()
			.rposition(Checkpoint::is_barrier)
			.map_or(0, |i| i + 1);
		history.take(mutable_start)
	}

	/// Creates a new span that includes this checkpoint and all other
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

	/// Returns tuples of signer and nonce for each transaction in this
	/// checkpoint.
	fn nonces(&self) -> Vec<(Address, u64)>;

	/// Returns a list of all unique signers in this checkpoint.
	fn signers(&self) -> Vec<Address>;

	/// For checkpoints that are not barriers, returns the transaction or bundle
	/// hash.
	fn hash(&self) -> Option<B256>;

	/// Returns `true` if this checkpoint has any transactions with a
	/// non-successful execution outcome.
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

	/// Returns a span starting at the last checkpoint tagged with `tag`.
	/// Returns `None` if no such tag exists in history.
	fn history_since_last_tag(&self, tag: &str) -> Option<Span<P>> {
		let history = self.history();
		let start = history.iter().rposition(|cp| cp.is_tagged(tag))?;
		Some(history.skip(start))
	}

	/// Returns a span starting at the first checkpoint tagged with `tag`.
	/// Returns `None` if no such tag exists in history.
	fn history_since_first_tag(&self, tag: &str) -> Option<Span<P>> {
		let history = self.history();
		let start = history.iter().position(|cp| cp.is_tagged(tag))?;
		Some(history.skip(start))
	}
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
		self
			.transactions()
			.iter()
			.any(|tx| tx.blob_gas_used().is_some())
	}

	//// Iterates over all blob transactions in the checkpoint.
	fn blobs(&self) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self
			.transactions()
			.iter()
			.filter(|tx| tx.blob_gas_used().is_some())
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

	/// Check if the checkpoint contains a transaction with a given hash.
	fn contains(&self, tx_hash: impl Into<TxHash>) -> bool {
		let hash = tx_hash.into();
		self.transactions().iter().any(|tx| *tx.tx_hash() == hash)
	}

	/// Creates a new span that includes this checkpoint and all other
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

	/// Returns tuples of signer and nonce for each transaction in this
	/// checkpoint.
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

	/// Returns `true` if this checkpoint has any transactions with a
	/// non-successful execution outcome.
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

	/// Returns the timestamp of the initial checkpoint that was created at the
	/// very beginning of the payload building process. It is the timestamp if
	/// the first barrier checkpoint in the history of this checkpoint that is
	/// created by `Checkpoint::new_at_block`.
	fn building_since(&self) -> Instant {
		self.root().created_at()
	}
}
