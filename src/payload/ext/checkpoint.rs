use {
	crate::{
		alloy::{
			consensus::transaction::{Transaction, TxHashRef},
			primitives::{Address, B256, TxHash, U256},
		},
		prelude::*,
		reth::{
			errors::ProviderError,
			primitives::Recovered,
			revm::{DatabaseRef, db::BundleState},
		},
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

	/// Returns a span starting at the first checkpoint that matches the provided
	/// context. Returns `None` if no such context exists in history.
	fn history_since_first_context(
		&self,
		context: &P::CheckpointContext,
	) -> Option<Span<P>> {
		let history = self.history();
		let start = history.iter().position(|cp| cp.has_context(context))?;
		Some(history.skip(start))
	}

	/// Returns a span starting at the last checkpoint that matches the provided
	/// context. Returns `None` if no such context exists in history.
	fn history_since_last_context(
		&self,
		context: &P::CheckpointContext,
	) -> Option<Span<P>> {
		let history = self.history();
		let start = history.iter().rposition(|cp| cp.has_context(context))?;
		Some(history.skip(start))
	}

	/// Account nonces changed after transactions execution.
	/// If transactions changes nonces nonces from N to N+1 this would return N+1.
	fn changed_nonces(&self) -> Vec<(Address, u64)>;
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

	fn changed_nonces(&self) -> Vec<(Address, u64)> {
		let Some(res) = self.result() else {
			return Vec::new();
		};
		extract_changed_nonces_for_executable(res.state())
	}
}

/// Get changed nonces from bundle state created as a result of execution of one
/// executable
fn extract_changed_nonces_for_executable(
	bundle_state: &BundleState,
) -> Vec<(Address, u64)> {
	let mut result = Vec::new();
	for (address, data) in bundle_state.state() {
		let old_nonce = data
			.original_info
			.as_ref()
			.map(|a| a.nonce)
			.unwrap_or_default();
		let new_nonce = data.info.as_ref().map(|a| a.nonce).unwrap_or_default();
		if old_nonce == new_nonce {
			continue;
		}
		result.push((*address, new_nonce));
	}
	result
}

#[cfg(test)]
mod tests {
	use {
		crate::{
			alloy::primitives::{Address, U256},
			payload::{Checkpoint, CheckpointExt},
			prelude::{BlockContext, Ethereum},
			test_utils::{BlockContextMocked, FundedAccounts, transfer_tx},
		},
		std::{
			thread,
			time::{Duration, Instant},
		},
	};

	#[test]
	fn test_new_at_block() {
		let block = BlockContext::<Ethereum>::mocked();
		let cp = Checkpoint::new_at_block(block);

		let cp2 = cp.barrier();
		let cp3 = cp2.barrier();

		assert!(cp.is_empty());
		assert_eq!(cp2.root(), cp);
		assert_eq!(cp3.root(), cp);

		assert_eq!(cp.gas_used(), 0);
		assert_eq!(cp.cumulative_gas_used(), 0);

		assert_eq!(cp.effective_tip_per_gas(), 0);
		assert!(!cp.has_blobs());
		assert_eq!(cp.blob_gas_used(), Some(0));
		assert_eq!(cp.cumulative_blob_gas_used(), 0);

		let span1 = cp3.to(&cp).unwrap();
		let span2 = cp.to(&cp3).unwrap();

		assert_eq!(span1.len(), span2.len());
		for i in 0..span2.len() {
			assert_eq!(span1.at(i), span2.at(i));
		}
		assert_eq!(span1.len(), 3);
		assert_eq!(span2.len(), 3);

		let addr = Address::ZERO;
		assert_eq!(cp.balance_of(addr).unwrap(), U256::ZERO);
		assert_eq!(cp.nonce_of(addr).unwrap(), 0);

		assert!(cp.signers().is_empty());
		assert!(cp.nonces().is_empty());

		assert_eq!(cp.hash(), None);
		assert!(!cp.is_bundle());
		assert!(!cp.has_failures());
		assert_eq!(cp.failed_txs().count(), 0);

		let random_addr = Address::random();
		assert_eq!(
			cp.balance_of(random_addr).unwrap(),
			U256::ZERO,
			"Nonexistent account should have zero balance"
		);

		assert_eq!(
			cp.nonce_of(random_addr).unwrap(),
			0,
			"Nonexistent account should have zero nonce"
		);
	}

	#[test]
	fn test_contains_is_false_without_txs() {
		let block = BlockContext::<Ethereum>::mocked();
		let cp1 = Checkpoint::new_at_block(block);

		let tx1 = transfer_tx::<Ethereum>(
			&FundedAccounts::signer(0),
			0,
			U256::from(50_000u64),
		);
		let tx1_hash = *tx1.hash();
		assert!(!cp1.contains(tx1_hash));
		let cp2 = cp1.apply(tx1).unwrap();

		assert!(cp2.contains(tx1_hash));
	}

	#[test]
	fn test_history_timestamps() {
		let block = BlockContext::<Ethereum>::mocked();
		let cp1 = Checkpoint::new_at_block(block);

		thread::sleep(Duration::from_millis(5));

		let cp2 = cp1.barrier();

		assert!(cp2.building_since() <= Instant::now());
		assert!(cp2.building_since() >= cp1.created_at());
	}

	#[test]
	fn test_to_self() {
		let block = BlockContext::<Ethereum>::mocked();
		let cp = Checkpoint::new_at_block(block);

		// to(self, self) should produce a span of length 1 containing the
		// checkpoint itself
		let span = cp.to(&cp).expect("to(self,self) must succeed");
		assert_eq!(span.len(), 1);
		assert_eq!(*span.at(0).unwrap(), cp);
	}

	#[test]
	fn test_to_non_linear_error() {
		let block_a = BlockContext::<Ethereum>::mocked();
		let block_b = BlockContext::<Ethereum>::mocked();

		let cp_a = Checkpoint::new_at_block(block_a);
		let cp_b = Checkpoint::new_at_block(block_b);

		// They are not on the same linear history, so to should return an Err.
		assert!(cp_a.to(&cp_b).is_err());
		assert!(cp_b.to(&cp_a).is_err());
	}

	#[test]
	fn test_to_includes_all_intermediates_and_is_linear() {
		let block = BlockContext::<Ethereum>::mocked();
		let base = Checkpoint::new_at_block(block);

		// base -> x -> y
		let tx_x =
			transfer_tx::<Ethereum>(&FundedAccounts::signer(0), 0, U256::from(10u64));
		let x = base.apply(tx_x).unwrap();

		let tx_y =
			transfer_tx::<Ethereum>(&FundedAccounts::signer(1), 0, U256::from(20u64));
		let y = x.apply(tx_y).unwrap();

		let x_barrier = x.barrier();
		let y_barrier = y.barrier();

		// `to` between base and y_barrier should include `base``, `x` (or
		// x_barrier), `y` (or y_barrier)
		let span_by = y_barrier
			.to(&base)
			.expect("to should succeed for linear history");
		let collected: Vec<Checkpoint<Ethereum>> = (0..span_by.len())
			.map(|i| span_by.at(i).unwrap().clone())
			.collect();

		assert!(collected.contains(&base));
		assert!(collected.contains(&y_barrier));
		assert!(collected.iter().any(|cp| *cp == x || *cp == x_barrier));
	}

	#[test]
	fn test_to_different_roots_error() {
		let block1 = BlockContext::<Ethereum>::mocked();
		let block2 = BlockContext::<Ethereum>::mocked();

		let root1 = Checkpoint::new_at_block(block1);
		let root2 = Checkpoint::new_at_block(block2);

		let tx =
			transfer_tx::<Ethereum>(&FundedAccounts::signer(0), 0, U256::from(5u64));
		let root1_child = root1.apply(tx).unwrap();

		assert!(root1_child.to(&root2).is_err());
		assert!(root2.to(&root1_child).is_err());
	}

	#[test]
	fn test_effective_tip_checkpoint() {
		let block = BlockContext::<Ethereum>::mocked();
		let cp = Checkpoint::new_at_block(block);
		assert_eq!(
			cp.effective_tip_per_gas(),
			0,
			"Empty checkpoint should have zero tip"
		);

		let tx = transfer_tx::<Ethereum>(
			&FundedAccounts::signer(0),
			0,
			U256::from(100u64),
		);
		let cp2 = cp.apply(tx.clone()).unwrap();

		let tip = cp2.effective_tip_per_gas();
		assert!(tip > 0, "Transaction should have positive effective tip");
	}

	#[test]
	fn test_history_staging_no_barrier() {
		let block = BlockContext::<Ethereum>::mocked();
		let base = Checkpoint::new_at_block(block);

		let tx1 =
			transfer_tx::<Ethereum>(&FundedAccounts::signer(0), 0, U256::from(50u64));
		let cp1 = base.apply(tx1).unwrap();

		let tx2 =
			transfer_tx::<Ethereum>(&FundedAccounts::signer(1), 0, U256::from(75u64));
		let cp2 = cp1.apply(tx2).unwrap();

		let staging = cp2.history_staging();
		let full = cp2.history();

		assert_eq!(
			staging.len(),
			full.len(),
			"Without barriers, staging should equal full history"
		);
	}

	#[test]
	fn test_history_staging_with_barrier() {
		let block = BlockContext::<Ethereum>::mocked();
		let base = Checkpoint::new_at_block(block);

		let tx1 =
			transfer_tx::<Ethereum>(&FundedAccounts::signer(0), 0, U256::from(50u64));
		let cp1 = base.apply(tx1).unwrap();

		let barrier = cp1.barrier();

		let tx2 =
			transfer_tx::<Ethereum>(&FundedAccounts::signer(1), 0, U256::from(75u64));
		let cp2 = barrier.apply(tx2).unwrap();

		let staging = cp2.history_staging();

		// Staging should only include checkpoints after the barrier
		assert!(
			staging.len() < cp2.history().len(),
			"Staging should be shorter than full history"
		);
		let sealed = cp2.history_sealed();

		// Sealed should include everything up to and including the barrier
		assert!(
			!sealed.is_empty(),
			"Sealed should include checkpoints up to barrier"
		);
	}
}
