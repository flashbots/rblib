use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::{crypto::RecoveryError, transaction::TxHashRef},
		primitives::{B256, TxHash},
	},
	reth::{
		errors::ProviderError,
		ethereum::primitives::SignedTransaction,
		evm::{
			ConfigureEvm,
			Evm,
			revm::{
				DatabaseCommit,
				DatabaseRef,
				context::result::ExecResultAndState,
			},
		},
		primitives::Recovered,
		revm::{
			State,
			db::{
				BundleState,
				WrapDatabaseRef,
				states::bundle_state::BundleRetention,
			},
		},
		transaction_pool::PoolTransaction,
	},
	std::fmt::Debug,
};

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError<P: Platform> {
	#[error("Invalid signature: {0}")]
	InvalidSignature(#[from] RecoveryError),

	#[error("Invalid transaction: {0}")]
	InvalidTransaction(types::EvmError<P, ProviderError>),

	#[error("Invalid transaction {0} cannot be dropped from bundle: {1}")]
	InvalidBundleTransaction(TxHash, types::EvmError<P, ProviderError>),

	#[error("Transaction {0} in the bundle is not allowed to revert.")]
	BundleTransactionReverted(TxHash),

	#[error("Invalid bundle post-execution state: {0}")]
	InvalidBundlePostExecutionState(types::BundlePostExecutionError<P>),

	#[error("Bundle is not eligible for execution in this block")]
	IneligibleBundle(Eligibility),
}

/// Describes an atomic unit of execution that can be used to create a state
/// transition checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub enum Executable<P: Platform> {
	// Individual transaction
	Transaction(Recovered<types::Transaction<P>>),

	// A bundle of transactions with metadata and behaviors.
	Bundle(types::Bundle<P>),
}

impl<P: Platform> Executable<P> {
	/// Executes this executable as a single unit of state transition and returns
	/// the outcome of the execution along with all state changes. If the
	/// executable is invalid, no execution result will be produced.
	///
	/// For details on what makes an executable invalid see the
	/// [`execute_transaction`] and [`execute_bundle`] methods.
	pub fn execute<DB>(
		self,
		block: &BlockContext<P>,
		db: &DB,
	) -> Result<ExecutionResult<P>, ExecutionError<P>>
	where
		DB: DatabaseRef<Error = ProviderError> + Debug,
	{
		match self {
			Self::Bundle(bundle) => Self::execute_bundle(bundle, block, db),
			Self::Transaction(tx) => Self::execute_transaction(tx, block, db)
				.map_err(ExecutionError::InvalidTransaction),
		}
	}

	/// Executes a single transaction and returns the outcome of the execution
	/// along with all state changes. This output is used to create a state
	/// checkpoint.
	///
	/// Notes:
	/// - Transactions that are invalid and cause EVM failures will not produce an
	///   execution result.
	///
	/// - Transactions that fail gracefully (revert or halt) will produce an
	///   execution result and state changes. It is up to higher levels of the
	///   system to decide what to do with such transactions, e.g., whether to
	///   remove them from the payload or not (see [`RevertProtection`]).
	fn execute_transaction<DB>(
		tx: Recovered<types::Transaction<P>>,
		block: &BlockContext<P>,
		db: &DB,
	) -> Result<ExecutionResult<P>, types::EvmError<P, ProviderError>>
	where
		DB: DatabaseRef<Error = ProviderError> + Debug,
	{
		let mut state = State::builder()
			.with_database(WrapDatabaseRef(db))
			.with_bundle_update()
			.build();

		let result = block
			.evm_config()
			.evm_with_env(&mut state, block.evm_env().clone())
			.transact_commit(&tx)?;

		state.merge_transitions(BundleRetention::Reverts);

		Ok(ExecutionResult {
			source: Executable::Transaction(tx),
			results: vec![result],
			state: state.take_bundle(),
		})
	}

	/// Executes a bundle of transactions and returns the execution outcome of all
	/// transactions in the bundle along with the aggregate of all state changes.
	///
	/// Notes:
	/// - Bundles that are not eligible for execution in the current block are
	///   considered invalid, and no execution result will be produced.
	///
	/// - All transactions in the bundle are executed in the order in which they
	///   were defined in the bundle.
	///
	/// - Each transaction is executed on the state produced by the previous
	///   transaction in the bundle.
	///
	/// - First transaction in the bundle is executed on the state of the
	///   checkpoint that we are building on.
	///
	/// - Transactions that cause EVM errors will invalidate the bundle, and no
	///   execution result will be produced (similar behavior to invalid loose
	///   txs). Bundle transaction can be marked optional [`Bundle::is_optional`],
	///   and invalid outcomes are handled differently:
	///     - If the invalid transaction is optional, a new version of the bundle
	///       will be created without the invalid transaction by removing it
	///       through [`Bundle::without_transaction`].
	///     - If removing the invalid optional transaction results in an empty
	///       bundle, the bundle will be considered invalid and no execution
	///       result will be produced.
	///
	/// - Transactions that fail gracefully (revert or halt) and are not optional
	///   will invalidate the bundle, and no execution result will be produced.
	///   Bundle transaction can be marked as allowed to fail
	///   [`Bundle::is_allowed_to_fail`], and failure outcomes are handled
	///   differently:
	///     - If the bundle allows the failing transaction to fail, the bundle
	///       will still be considered valid. The execution result will be
	///       produced, including this failed transaction. State changes from the
	///       failed transaction will be included in the aggregate state, e.g.,
	///       gas used, nonces incremented, etc. Cleaning up transactions that are
	///       allowed to fail and are optional from a bundle is beyond the scope
	///       of this method. This is implemented by higher levels of the system,
	///       such as the [`RevertProtection`] step in the pipelines API.
	///     - If the bundle does not allow this failed transaction to fail, but
	///       the transaction is optional, then it will be removed from the
	///       bundle. The bundle stays valid.
	///
	/// See truth table:
	/// | success | `allowed_to_fail` | optional | Action  |
	/// | ------: | :---------------: | :------: | :------ |
	/// |    true |    *don’t care*   |   *any*  | include |
	/// |   false |        true       |   *any*  | include |
	/// |   false |       false       |   true   | discard |
	/// |   false |       false       |   false  | error   |
	///
	/// - At the end of the bundle execution, the bundle implementation will have
	///   a chance to validate any other platform-specific post-execution
	///   requirements. For example, the bundle may require that the state after
	///   the execution has a certain balance in some account, etc. If this check
	///   fails, the bundle will be considered invalid, and no execution result
	///   will be produced.
	fn execute_bundle<DB>(
		bundle: types::Bundle<P>,
		block: &BlockContext<P>,
		db: &DB,
	) -> Result<ExecutionResult<P>, ExecutionError<P>>
	where
		DB: DatabaseRef<Error = ProviderError> + Debug,
	{
		let eligible = bundle.is_eligible(block);
		if !eligible {
			return Err(ExecutionError::IneligibleBundle(eligible));
		}

		let evm_env = block.evm_env();
		let evm_config = block.evm_config();
		let mut db = State::builder()
			.with_database(WrapDatabaseRef(db))
			.with_bundle_update()
			.build();

		let mut discarded = Vec::new();
		let mut results = Vec::with_capacity(bundle.transactions().len());

		for transaction in bundle.transactions_encoded() {
			let tx_hash = *transaction.tx_hash();
			let optional = bundle.is_optional(&tx_hash);
			let allowed_to_fail = bundle.is_allowed_to_fail(&tx_hash);

			let result = evm_config
				.evm_with_env(&mut db, evm_env.clone())
				.transact(&transaction);

			match result {
				// Valid transaction or allowed to fail: include it in the bundle
				Ok(ExecResultAndState { result, state })
					if result.is_success() || allowed_to_fail =>
				{
					results.push(result);
					db.commit(state);
				}
				// Optional failing transaction, not allowed to fail
				// or optional invalid transaction: discard it
				Ok(_) | Err(_) if optional => {
					discarded.push(tx_hash);
				}
				// Non-Optional failing transaction, not allowed to fail: invalidate the
				// bundle
				Ok(_) => {
					return Err(ExecutionError::BundleTransactionReverted(tx_hash));
				}
				// Non-Optional invalid transaction: invalidate the bundle
				Err(err) => {
					return Err(ExecutionError::InvalidBundleTransaction(tx_hash, err));
				}
			}
		}

		// reduce the bundle by removing discarded transactions
		let bundle = discarded
			.into_iter()
			.fold(bundle, |b, tx| b.without_transaction(tx));

		// extract all the state changes that were made by executing
		// transactions in this bundle.
		db.merge_transitions(BundleRetention::Reverts);
		let state = db.take_bundle();

		// run the optional post-execution validation of the bundle.
		bundle
			.validate_post_execution(&state, block)
			.map_err(ExecutionError::InvalidBundlePostExecutionState)?;

		Ok(ExecutionResult {
			source: Executable::Bundle(bundle),
			results,
			state,
		})
	}
}

impl<P: Platform> Executable<P> {
	/// Returns all transactions that make up this executable.
	pub fn transactions(&self) -> &[Recovered<types::Transaction<P>>] {
		match self {
			Self::Transaction(tx) => std::slice::from_ref(tx),
			Self::Bundle(bundle) => bundle.transactions(),
		}
	}

	pub const fn is_transaction(&self) -> bool {
		matches!(self, Self::Transaction(_))
	}

	pub const fn is_bundle(&self) -> bool {
		matches!(self, Self::Bundle(_))
	}

	pub fn hash(&self) -> B256 {
		match self {
			Self::Transaction(tx) => *tx.tx_hash(),
			Self::Bundle(bundle) => bundle.hash(),
		}
	}
}

/// Convenience trait that allows all types that can be executed to be used as a
/// parameter to the `Checkpoint::apply` method.
pub trait IntoExecutable<P: Platform, S = ()> {
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError>;
}

/// Transactions can be converted into an executable as long as they have a
/// valid recoverable signature.
impl<P: Platform> IntoExecutable<P, Variant<0>> for types::Transaction<P> {
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		SignedTransaction::try_into_recovered(self)
			.map(Executable::Transaction)
			.map_err(|_| RecoveryError::new())
	}
}

/// Transactions from the transaction pool can be converted infallibly into
/// an executable because the transaction pool discards transactions
/// that have invalid signatures.
impl<P: Platform> IntoExecutable<P, Variant<1>>
	for types::PooledTransaction<P>
{
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		Ok(Executable::Transaction(self.into_consensus()))
	}
}

/// Signature-recovered individual transactions are always infallibly
/// convertible into an executable.
impl<P: Platform> IntoExecutable<P, Variant<2>>
	for Recovered<types::Transaction<P>>
{
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		Ok(Executable::Transaction(self))
	}
}

/// Bundles are also convertible into an executable infallibly.
/// Signature recovery is part of the bundle assembly logic.
impl<P: Platform> IntoExecutable<P, Variant<3>> for types::Bundle<P> {
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		Ok(Executable::Bundle(self))
	}
}

/// Already converted executables
impl<P: Platform> IntoExecutable<P, Variant<4>> for Executable<P> {
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		Ok(self)
	}
}

/// Another checkpoint content
impl<P: Platform> IntoExecutable<P, Variant<5>> for Checkpoint<P> {
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		(&self).try_into_executable()
	}
}

impl<P: Platform> IntoExecutable<P, Variant<6>> for &Checkpoint<P> {
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		if let Some(tx) = self.as_transaction() {
			Ok(Executable::Transaction(tx.clone()))
		} else if let Some(bundle) = self.as_bundle() {
			Ok(Executable::Bundle(bundle.clone()))
		} else {
			Err(RecoveryError::new())
		}
	}
}

/// From EIP-2718 transaction envelope to executable.
impl<P: PlatformWithRpcTypes> IntoExecutable<P, Variant<7>>
	for types::TxEnvelope<P>
{
	fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		let tx: types::Transaction<P> = self.into();
		tx.try_into_executable()
	}
}

/// This trait represents the overall result of executing a transaction or a
/// bundle of transactions.
///
/// Types implementing this trait provide access to the individual results of
/// transaction executions that make up this overall result.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionResult<P: Platform> {
	/// The executable used to produce this result.
	source: Executable<P>,

	/// For transactions this is guaranteed to be a single-element vector,
	/// for bundles this is guaranteed to be a vector of results for each
	/// transaction in the bundle.
	results: Vec<types::TransactionExecutionResult<P>>,

	/// The aggregated state executing all transactions from the source.
	state: BundleState,
}

impl<P: Platform> ExecutionResult<P> {
	/// Returns the executable used to produce this result.
	pub const fn source(&self) -> &Executable<P> {
		&self.source
	}

	/// Returns the aggregate state changes made by executing the transactions in
	/// this execution unit.
	pub const fn state(&self) -> &BundleState {
		&self.state
	}

	/// Access to the individual transaction results that make up this execution
	/// result.
	///
	/// For transactions, this will return a single-element slice containing the
	/// transaction's execution result. For bundles, this will return a slice of
	/// execution results for each transaction in the bundle.
	pub const fn results(&self) -> &[types::TransactionExecutionResult<P>] {
		self.results.as_slice()
	}

	/// Returns individual transactions executed as part of this execution unit.
	pub fn transactions(&self) -> &[Recovered<types::Transaction<P>>] {
		self.source().transactions()
	}

	/// Returns the cumulative gas used by the execution of this transaction or
	/// bundle.
	pub fn gas_used(&self) -> u64 {
		self.results.iter().map(|r| r.gas_used()).sum()
	}
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::test_utils::{BlockContextMocked, test_bundle, test_tx, test_txs},
	};

	#[test]
	fn test_executable_transaction_returns_single_transaction() {
		let tx = test_tx::<Ethereum>(0, 0);
		let executable = Executable::<Ethereum>::Transaction(tx.clone());

		assert_eq!(executable.transactions().len(), 1);
		assert_eq!(executable.transactions()[0], tx);
		assert!(executable.is_transaction());
		assert!(!executable.is_bundle());
	}

	#[test]
	fn test_executable_bundle_returns_all_transactions() {
		let (bundle, txs) = test_bundle::<Ethereum>(0, 0);
		let executable = Executable::<Ethereum>::Bundle(bundle);

		assert_eq!(executable.transactions().len(), txs.len());
		assert_eq!(executable.transactions(), txs.as_slice());
		assert!(!executable.is_transaction());
		assert!(executable.is_bundle());
	}

	#[test]
	fn test_execute_transaction_success() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);

		let result =
			Executable::execute_transaction(tx.clone(), &block, &checkpoint);

		let exec_result = result.unwrap();
		assert_eq!(exec_result.results().len(), 1);
		assert_eq!(exec_result.transactions().len(), 1);
		assert_eq!(exec_result.transactions()[0], tx);
		assert!(exec_result.results()[0].is_success());
	}

	#[test]
	fn test_execute_transaction_produces_state_changes() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);

		let result = Executable::execute_transaction(tx, &block, &checkpoint);

		let exec_result = result.unwrap();
		assert!(!exec_result.state().is_empty());
		assert!(exec_result.gas_used() > 0);
	}

	#[test]
	fn test_execute_via_execute_method() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);
		let executable = Executable::<Ethereum>::Transaction(tx);

		let result = executable.execute(&block, &checkpoint);

		assert_eq!(result.unwrap().results().len(), 1);
	}

	#[test]
	fn test_execute_bundle_all_successful() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let (bundle, txs) = test_bundle::<Ethereum>(0, 0);

		let result = Executable::execute_bundle(bundle, &block, &checkpoint);

		let exec_result = result.unwrap();
		assert_eq!(exec_result.results().len(), txs.len());
		assert!(exec_result.results().iter().all(|r| r.is_success()));
		assert_eq!(exec_result.transactions().len(), txs.len());
		assert_eq!(exec_result.transactions(), txs.as_slice());
	}

	#[test]
	fn test_execute_bundle_aggregates_gas() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let (bundle, _) = test_bundle::<Ethereum>(0, 0);

		let result = Executable::execute_bundle(bundle, &block, &checkpoint);

		let exec_result = result.unwrap();
		let total_gas = exec_result.gas_used();
		let sum_gas: u64 = exec_result.results().iter().map(|r| r.gas_used()).sum();
		assert_eq!(total_gas, sum_gas);
		assert!(total_gas > 0);
	}

	#[test]
	fn test_execute_bundle_sequential_execution() {
		// Each transaction in a bundle executes on the state from the previous
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		// Use the same account for all transactions to test sequential nonces
		let txs = test_txs::<Ethereum>(0, 0, 3);
		let bundle = FlashbotsBundle::<Ethereum>::default()
			.with_transaction(txs[0].clone())
			.with_transaction(txs[1].clone())
			.with_transaction(txs[2].clone());

		let result = Executable::execute_bundle(bundle, &block, &checkpoint);

		let exec_result = result.unwrap();
		assert_eq!(exec_result.results().len(), txs.len());
		assert!(exec_result.results().iter().all(|r| r.is_success()));
	}

	#[test]
	fn test_into_executable_from_recovered_transaction() {
		let tx = test_tx::<Ethereum>(0, 0);
		let result: Result<Executable<Ethereum>, _> =
			IntoExecutable::<Ethereum, Variant<2>>::try_into_executable(tx.clone());

		let executable = result.unwrap();
		assert!(executable.is_transaction());
		assert_eq!(executable.transactions()[0], tx);
	}

	#[test]
	fn test_into_executable_from_bundle() {
		let (bundle, _) = test_bundle::<Ethereum>(0, 0);
		let result: Result<Executable<Ethereum>, _> =
			IntoExecutable::<Ethereum, Variant<3>>::try_into_executable(bundle);

		assert!(result.unwrap().is_bundle());
	}

	#[test]
	fn test_into_executable_from_executable() {
		let tx = test_tx::<Ethereum>(0, 0);
		let executable = Executable::<Ethereum>::Transaction(tx);
		let result: Result<Executable<Ethereum>, _> =
			IntoExecutable::<Ethereum, Variant<4>>::try_into_executable(
				executable.clone(),
			);

		assert_eq!(result.unwrap(), executable);
	}

	#[test]
	fn test_into_executable_from_checkpoint() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);
		let checkpoint_with_tx = checkpoint.apply(tx.clone()).unwrap();

		let result: Result<Executable<Ethereum>, _> =
			IntoExecutable::<Ethereum, Variant<5>>::try_into_executable(
				checkpoint_with_tx,
			);

		let executable = result.unwrap();
		assert!(executable.is_transaction());
		assert_eq!(executable.transactions()[0], tx);
	}

	#[test]
	fn test_into_executable_from_checkpoint_ref() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);
		let checkpoint_with_tx = checkpoint.apply(tx.clone()).unwrap();

		let result: Result<Executable<Ethereum>, _> =
			IntoExecutable::<Ethereum, Variant<6>>::try_into_executable(
				&checkpoint_with_tx,
			);

		let executable = result.unwrap();
		assert!(executable.is_transaction());
		assert_eq!(executable.transactions()[0], tx);
	}

	#[test]
	fn test_into_executable_from_barrier_checkpoint_fails() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let barrier = checkpoint.barrier();

		let result: Result<Executable<Ethereum>, _> =
			IntoExecutable::<Ethereum, Variant<6>>::try_into_executable(&barrier);

		assert!(result.is_err());
	}

	#[test]
	fn test_execution_result_source() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);
		let executable = Executable::<Ethereum>::Transaction(tx.clone());

		let result = executable.execute(&block, &checkpoint).unwrap();

		match result.source() {
			Executable::Transaction(result_tx) => assert_eq!(*result_tx, tx),
			Executable::Bundle(_) => panic!("Expected transaction source"),
		}
	}

	#[test]
	fn test_execution_result_transactions() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let (bundle, txs) = test_bundle::<Ethereum>(0, 0);
		let executable = Executable::<Ethereum>::Bundle(bundle);

		let result = executable.execute(&block, &checkpoint).unwrap();

		assert_eq!(result.transactions(), txs.as_slice());
	}

	#[test]
	fn test_executable_hash_transaction() {
		let tx = test_tx::<Ethereum>(0, 0);
		let expected_hash = *tx.tx_hash();
		let executable = Executable::<Ethereum>::Transaction(tx);

		assert_eq!(executable.hash(), expected_hash);
	}

	#[test]
	fn test_executable_hash_bundle() {
		let (bundle, _) = test_bundle::<Ethereum>(0, 0);
		let expected_hash = bundle.hash();
		let executable = Executable::<Ethereum>::Bundle(bundle);

		assert_eq!(executable.hash(), expected_hash);
	}

	#[test]
	fn test_execution_error_invalid_signature_display() {
		let err =
			ExecutionError::<Ethereum>::InvalidSignature(RecoveryError::new());
		let display = format!("{err}");
		assert!(display.contains("Invalid signature"));
	}

	#[test]
	fn test_execution_result_state_is_bundle_state() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);

		let result =
			Executable::execute_transaction(tx, &block, &checkpoint).unwrap();

		// State should be a BundleState with changes
		assert!(!result.state().is_empty());
	}

	#[test]
	fn test_execution_result_clone() {
		let block = BlockContext::<Ethereum>::mocked();
		let checkpoint = block.start();
		let tx = test_tx::<Ethereum>(0, 0);

		let result =
			Executable::execute_transaction(tx, &block, &checkpoint).unwrap();
		let cloned = result.clone();

		assert_eq!(result, cloned);
	}
}
