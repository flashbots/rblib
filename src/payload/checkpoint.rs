use {
	super::exec::IntoExecutable,
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::{crypto::RecoveryError, transaction::TxHashRef},
		primitives::{Address, B256, StorageValue},
	},
	core::fmt::{Debug, Display},
	reth::{
		errors::ProviderError,
		primitives::Recovered,
		revm::{
			DatabaseRef,
			db::BundleState,
			primitives::StorageKey,
			state::{AccountInfo, Bytecode},
		},
	},
	std::{iter::Successors, sync::Arc, time::Instant},
	thiserror::Error,
};

#[derive(Debug, Error)]
pub enum Error<P: Platform> {
	#[error("Failed to recover signature for transaction")]
	SignatureRecovery(#[from] RecoveryError),

	#[error("Evm execution error: {0}")]
	Evm(types::EvmError<P, ProviderError>),
}

/// Checkpoints represent an atomic incremental change in the payload building
/// process.
///
/// Notes:
///  - There is no public API to create a checkpoint directly. Checkpoints are
///    created from the [`BlockContext`] when it starts a new payload building
///    process or by mutations applied to an already existing checkpoint.
///
///  - Checkpoints contain all the information needed to assemble a full block
///    payload. However, they cannot be used directly to assemble a block. The
///    block assembly process is very node-specific and is part of the pipelines
///    api, which has more info and access to the underlying node facilities.
///
///  - Checkpoints are immutable, meaning that once a checkpoint is created, it
///    cannot be changed. Instead, new checkpoints can be created on top of the
///    existing ones, forming a chain of checkpoints.
///
///  - Checkpoints may represent forks in the payload building process. Two
///    checkpoints can share a common ancestor without having a linear history
///    between them. Each of the diverging checkpoints can be used to build
///    alternative versions of the payload.
///
///  - Checkpoints are inexpensive to clone, discard and move around. However,
///    they are expensive to create, as they require executing transactions
///    through the EVM and storing the resulting state changes.
///
///  - Checkpoints are thread-safe, Send + Sync + 'static.
///
///  - Checkpoints are always in a state that can be used to build a valid block
///    payload. You can't create checkpoints with invalid transactions (such as
///    invalid nonces, invalid signatures, etc.) that would invalidate the block
///    payload validity according to consensus rules.
///
///  - Checkpoints are state providers, meaning that any checkpoint can be used
///    as a database reference in an input to an EVM instance when simulating
///    transactions. The state of the checkpoint is the cumulative state of all
///    state mutations applied since the beginning of the block payload,
///    including the base state of the parent block of the block for which the
///    payload is being built.
pub struct Checkpoint<P: Platform> {
	inner: Arc<CheckpointInner<P>>,
}

/// Public read API
impl<P: Platform> Checkpoint<P> {
	/// Returns the number of checkpoints preceding this checkpoint from the
	/// beginning of the block payload.
	///
	/// Depth zero is when [`BlockContext::start`] is called, and the first
	/// checkpoint is created with no previous checkpoints.
	pub fn depth(&self) -> usize {
		self.inner.depth
	}

	/// Returns the timestamp when this checkpoint was created.
	pub fn created_at(&self) -> Instant {
		self.inner.created_at
	}

	/// Returns the previous checkpoint before the current checkpoint.
	///
	/// Using the previous checkpoint is equivalent to discarding the
	/// state mutations made in the current checkpoint.
	///
	/// There may be multiple parallel forks of the payload under construction,
	/// rooted at the same checkpoint.
	pub fn prev(&self) -> Option<Checkpoint<P>> {
		self.inner.prev.as_ref().map(|prev| Checkpoint {
			inner: Arc::clone(prev),
		})
	}

	/// Returns the block context at the root of the checkpoint.
	pub fn block(&self) -> &BlockContext<P> {
		&self.inner.block
	}

	/// The transactions that created this checkpoint.
	/// The returned slice is a view into all applied transactions in this
	/// checkpoint:
	/// - Empty if this checkpoint is a barrier or other non-transaction
	///   checkpoint.
	/// - Single transaction if this checkpoint was created by applying a single
	///   transaction.
	/// - Multiple transactions if this checkpoint represents a bundle.
	pub fn transactions(&self) -> &[Recovered<types::Transaction<P>>] {
		match &self.inner.mutation {
			Mutation::Barrier => &[],
			Mutation::Executable(result) => result.transactions(),
		}
	}

	/// The execution result(s) of the transaction(s) that created this
	/// checkpoint.
	pub fn result(&self) -> Option<&ExecutionResult<P>> {
		match &self.inner.mutation {
			Mutation::Barrier => None,
			Mutation::Executable(result) => Some(result),
		}
	}

	/// The state changes that occurred as a result of executing the
	/// transaction(s) that created this checkpoint.
	pub fn state(&self) -> Option<&BundleState> {
		match self.inner.mutation {
			Mutation::Barrier => None,
			Mutation::Executable(ref result) => Some(result.state()),
		}
	}

	/// Returns true if this checkpoint is a barrier checkpoint.
	pub fn is_barrier(&self) -> bool {
		matches!(self.inner.mutation, Mutation::Barrier)
	}

	/// Returns true if this checkpoint has the given tag.
	pub fn is_tagged(&self, tag: &str) -> bool {
		self.tag().is_some_and(|t| t == tag)
	}

	/// Returns the tag of this checkpoint, if any.
	pub fn tag(&self) -> Option<&str> {
		self.inner.tag.as_deref()
	}

	/// If this checkpoint is created from a single transaction, returns a
	/// reference to this transaction. Otherwise, returns `None`.
	pub fn as_transaction(&self) -> Option<&Recovered<types::Transaction<P>>> {
		if let Mutation::Executable(result) = &self.inner.mutation
			&& let Executable::Transaction(tx) = result.source()
		{
			return Some(tx);
		}
		None
	}

	/// If this checkpoint is created from a bundle, returns a reference to this
	/// bundle. Otherwise, returns `None`.
	pub fn as_bundle(&self) -> Option<&types::Bundle<P>> {
		if let Mutation::Executable(result) = &self.inner.mutation
			&& let Executable::Bundle(bundle) = result.source()
		{
			return Some(bundle);
		}
		None
	}
}

/// Public builder API
impl<P: Platform> Checkpoint<P> {
	/// Creates a new checkpoint on top of the current checkpoint by applying a
	/// transaction or a bundle of transactions. The execution will use the
	/// cumulative state of all checkpoints in the current checkpoint history as
	/// its state.
	pub fn apply<S>(
		&self,
		executable: impl IntoExecutable<P, S>,
	) -> Result<Self, ExecutionError<P>> {
		let mutation = Mutation::Executable(
			executable
				.try_into_executable()?
				.execute(self.block(), self)?,
		);
		Ok(self.apply_with(mutation, None))
	}

	/// Executes transaction on top of the current checkpoint. The execution will
	/// use the cumulative state of all checkpoints in the current checkpoint
	/// history as its state.
	pub fn simulate<S>(
		&self,
		executable: impl IntoExecutable<P, S>,
	) -> Result<ExecutionResult<P>, ExecutionError<P>> {
		executable
			.try_into_executable()?
			.simulate(self.block(), self)
	}

	/// Creates a new checkpoint on top of the current checkpoint and tags it.
	/// The execution will use the cumulative state of all checkpoints in the
	/// current checkpoint history as its state.
	pub fn apply_with_tag<S>(
		&self,
		executable: impl IntoExecutable<P, S>,
		tag: impl Into<Box<str>>,
	) -> Result<Self, ExecutionError<P>> {
		let mutation = Mutation::Executable(
			executable
				.try_into_executable()?
				.execute(self.block(), self)?,
		);
		Ok(self.apply_with(mutation, Some(tag.into())))
	}

	/// Creates a new checkpoint on top of the current checkpoint that introduces
	/// a barrier. This new checkpoint will be now considered the new beginning of
	/// staging history.
	#[must_use]
	pub fn barrier(&self) -> Self {
		Self::apply_with(self, Mutation::Barrier, None)
	}

	/// Creates a new tagged barrier checkpoint on top of the current checkpoint.
	#[must_use]
	pub fn barrier_with_tag(&self, tag: impl Into<Box<str>>) -> Self {
		Self::apply_with(self, Mutation::Barrier, Some(tag.into()))
	}

	/// Given this checkpoint, this method builds a new payload on top of this
	/// block base state that is ready to be handed back to the CL client as a
	/// response to the `ForkchoiceUpdated` request.
	pub fn build_payload(
		&self,
	) -> Result<types::BuiltPayload<P>, PayloadBuilderError> {
		P::build_payload(self.clone(), self.block().base_state())
	}
}

/// Internal API
impl<P: Platform> Checkpoint<P> {
	// Create a new checkpoint on top of the current one with the given mutation.
	// See public builder API.
	#[must_use]
	fn apply_with(&self, mutation: Mutation<P>, tag: Option<Box<str>>) -> Self {
		Self {
			inner: Arc::new(CheckpointInner {
				block: self.inner.block.clone(),
				prev: Some(Arc::clone(&self.inner)),
				depth: self.inner.depth + 1,
				mutation,
				created_at: Instant::now(),
				tag,
			}),
		}
	}

	/// Start a new checkpoint for an empty payload rooted at the
	/// state of the parent block of the block for which the payload is
	/// being built.
	#[must_use]
	pub(super) fn new_at_block(block: BlockContext<P>) -> Self {
		Self {
			inner: Arc::new(CheckpointInner {
				block,
				prev: None,
				depth: 0,
				mutation: Mutation::Barrier,
				created_at: Instant::now(),
				tag: None,
			}),
		}
	}

	/// Lazy iterator over historic checkpoints.
	/// Note that it is in reverse history order, starting from the latest applied
	/// checkpoint up to the first one.
	fn iter(&self) -> Successors<Self, fn(&Self) -> Option<Self>> {
		<&Self as IntoIterator>::into_iter(self)
	}
}

impl<P: Platform> IntoIterator for &Checkpoint<P> {
	type IntoIter =
		Successors<Checkpoint<P>, fn(&Checkpoint<P>) -> Option<Checkpoint<P>>>;
	type Item = Checkpoint<P>;

	fn into_iter(self) -> Self::IntoIter {
		std::iter::successors(Some(self.clone()), |cp| {
			cp.inner.prev.as_ref().map(|prev| Checkpoint {
				inner: Arc::clone(prev),
			})
		})
	}
}

/// Describes the type of state mutation that was applied to the previous
/// checkpoint to create this checkpoint.
#[derive(Debug, PartialEq)]
enum Mutation<P: Platform> {
	/// A checkpoint that indicates that any prior checkpoints are immutable and
	/// should not be discarded or reordered. An example of this would be placing
	/// a barrier after applying sequencer transactions to ensure that they do
	/// not get reordered by pipelines. Another example would be placing a barrier
	/// after every committed flashblock, to ensure that any steps in the pipeline
	/// do not modify the committed state of the payload in process.
	///
	/// If there are multiple barriers in the history, the last one is considered
	/// as the beginning of the staging history.
	///
	/// The very first checkpoint in the history is always a barrier, as it
	/// represents the baseline checkpoint that has no transactions in its
	/// history.
	Barrier,

	/// A checkpoint that was created by applying one executable item on top of
	/// the previous checkpoint. The executable item can be a single transaction
	/// or a bundle of transactions.
	Executable(ExecutionResult<P>),
}

struct CheckpointInner<P: Platform> {
	/// The block context for which this checkpoint was created.
	block: BlockContext<P>,

	/// The previous checkpoint in this chain of checkpoints, if any.
	prev: Option<Arc<Self>>,

	/// The number of checkpoints in the chain starting from the beginning of the
	/// block context.
	///
	/// Depth zero is when [`BlockContext::start`] is called, as the first
	/// checkpoint
	depth: usize,

	/// The mutation kind for the checkpoint.
	mutation: Mutation<P>,

	/// The timestamp when this checkpoint was created.
	created_at: Instant,

	/// Optional tag for this checkpoint. Tags are metadata used to mark
	/// checkpoints for later reference in history queries and display/debug
	/// output.
	tag: Option<Box<str>>,
}

/// Converts a checkpoint into a vector of transactions that were applied to
/// it.
impl<P: Platform> From<Checkpoint<P>> for Vec<types::Transaction<P>> {
	fn from(checkpoint: Checkpoint<P>) -> Self {
		checkpoint
			.history()
			.transactions()
			.map(Recovered::clone_inner)
			.collect()
	}
}

/// Any checkpoint can be used as a database reference for an EVM instance.
/// The state at a checkpoint is the cumulative aggregate of all state mutations
/// that occurred in the current checkpoint and all its ancestors on top of the
/// base state of the parent block of the block for which the payload is being
/// built.
impl<P: Platform> DatabaseRef for Checkpoint<P> {
	/// The database error type.
	type Error = ProviderError;

	/// Gets basic account information.
	fn basic_ref(
		&self,
		address: Address,
	) -> Result<Option<AccountInfo>, Self::Error> {
		// we want to probe the history of checkpoints in reverse order,
		// starting from the most recent one, to find the first checkpoint
		// that has touched the given address.

		if let Some(account) = self.iter().find_map(|checkpoint| {
			checkpoint
				.result()?
				.state()
				.account(&address)
				.and_then(|account| account.info.as_ref())
				.cloned()
		}) {
			return Ok(Some(account));
		}

		// none of the checkpoints priori to this have touched this address,
		// now we need to check if the account exists in the base state of the
		// block context.
		if let Some(acc) = self.block().base_state().basic_account(&address)? {
			return Ok(Some(acc.into()));
		}

		// account does not exist
		Ok(None)
	}

	/// Gets account code by its hash.
	fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
		// we want to probe the history of checkpoints in reverse order,
		// starting from the most recent one, to find the first checkpoint
		// that has created the code with the given hash.

		if let Some(code) = self
			.iter()
			.find_map(|checkpoint| checkpoint.result()?.state().bytecode(&code_hash))
		{
			return Ok(code);
		}

		// check if the code exists in the base state of the block context.
		Ok(
			self
				.block()
				.base_state()
				.bytecode_by_hash(&code_hash)?
				.unwrap_or_default()
				.0,
		)
	}

	/// Gets storage value of address at index.
	fn storage_ref(
		&self,
		address: Address,
		index: StorageKey,
	) -> Result<StorageValue, Self::Error> {
		// traverse checkpoint history looking for the first checkpoint that
		// has touched the given address.

		if let Some(value) = self.iter().find_map(|checkpoint| {
			checkpoint
				.result()?
				.state()
				.account(&address)
				.and_then(|account| account.storage.get(&index))
				.map(|slot| slot.present_value)
		}) {
			return Ok(value);
		}

		// none of the checkpoints prior to this have touched this address,
		// now we need to check if the account exists in the base state of the
		// block context.
		Ok(
			self
				.block()
				.base_state()
				.storage(address, index.into())?
				.unwrap_or_default(),
		)
	}

	/// Gets block hash by block number.
	fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
		Ok(
			self
				.block()
				.base_state()
				.block_hash(number)?
				.unwrap_or_default(),
		)
	}
}

impl<P: Platform> Clone for Checkpoint<P> {
	fn clone(&self) -> Self {
		Self {
			inner: Arc::clone(&self.inner),
		}
	}
}

impl<P: Platform> PartialEq for Checkpoint<P> {
	fn eq(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}
}

impl<P: Platform> Eq for Checkpoint<P> {}

impl<P: Platform> Debug for Checkpoint<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		f.debug_struct("Checkpoint")
			.field("depth", &self.depth())
			.field("block", &format!("{} + 1", self.block().parent().hash()))
			.field("tag", &self.tag())
			.field(
				"txs",
				&self
					.transactions()
					.iter()
					.map(|tx| tx.tx_hash())
					.collect::<Vec<_>>(),
			)
			.field("result", &self.result())
			.finish()
	}
}

impl<P: Platform> Display for Checkpoint<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		let tag_suffix = self
			.tag()
			.map(|tag| format!(" '{tag}'"))
			.unwrap_or_default();

		let Mutation::Executable(exec_result) = &self.inner.mutation else {
			if self.depth() == 0 {
				// this is the initial checkpoint
				return write!(f, "[{}] initial", self.depth());
			}

			// this is a barrier checkpoint, which has no transactions
			// applied to it.
			return match &self.inner.mutation {
				Mutation::Barrier => {
					write!(f, "[{}] barrier{}", self.depth(), tag_suffix)
				}
				Mutation::Executable(_) => {
					unreachable!("Executable variant handled above")
				}
			};
		};

		match exec_result.source() {
			Executable::Transaction(tx) => write!(
				f,
				"[{}] tx {} ({}, {} gas){}",
				self.depth(),
				tx.tx_hash(),
				match exec_result.results()[0] {
					types::TransactionExecutionResult::<P>::Success { .. } => "success",
					types::TransactionExecutionResult::<P>::Revert { .. } => "revert",
					types::TransactionExecutionResult::<P>::Halt { .. } => "halt",
				},
				self.gas_used(),
				tag_suffix,
			),
			Executable::Bundle(bundle) => write!(
				f,
				"[{}] (bundle {} txs, {} gas){}",
				self.depth(),
				bundle.transactions().len(),
				self.gas_used(),
				tag_suffix,
			),
		}
	}
}

#[cfg(test)]
mod tests {
	use {
		crate::{
			payload::checkpoint::{Checkpoint, IntoExecutable, Mutation},
			prelude::*,
			reth::primitives::Recovered,
			test_utils::{BlockContextMocked, test_bundle, test_tx, test_txs},
		},
		std::time::Instant,
	};

	/// Helper test function to apply multiple transactions on a checkpoint
	fn apply_multiple<P: PlatformWithRpcTypes>(
		root: Checkpoint<P>,
		txs: &[Recovered<types::Transaction<P>>],
	) -> Vec<Checkpoint<P>> {
		let mut cur = root;
		txs
			.iter()
			.map(|tx| {
				cur = cur.apply(tx.clone()).unwrap();
				cur.clone()
			})
			.collect()
	}

	mod internal {
		use super::*;
		#[test]
		fn test_new_at_block() {
			// test the initial checkpoint with private `Checkpoint::new_at_block`
			let block = BlockContext::<Ethereum>::mocked();

			let before = Instant::now();
			let checkpoint = Checkpoint::new_at_block(block.clone());
			let after = Instant::now();

			assert_eq!(checkpoint.block(), &block);
			assert!(checkpoint.prev().is_none());
			assert_eq!(checkpoint.depth(), 0);
			assert!(checkpoint.is_barrier());
			assert!((before..=after).contains(&checkpoint.created_at()));
		}

		#[test]
		fn test_apply_with() {
			// test the checkpoint obtained by application with private
			// `Checkpoint::apply_with`
			let block = BlockContext::<Ethereum>::mocked();
			let root = block.start();

			let before = Instant::now();
			let checkpoint = root.apply_with(Mutation::Barrier, None);
			let after = Instant::now();

			assert_eq!(checkpoint.block(), &block);
			assert_eq!(checkpoint.prev(), Some(root.clone()));
			assert_eq!(checkpoint.depth(), root.depth() + 1);
			assert!(checkpoint.is_barrier());
			assert!(checkpoint.tag().is_none());
			assert!((before..=after).contains(&checkpoint.created_at()));
		}
	}

	#[test]
	fn test_apply_barrier() {
		let block = BlockContext::<Ethereum>::mocked();
		let root = block.start();
		let barrier = root.barrier();

		assert!(barrier.result().is_none());
		assert!(barrier.state().is_none());
		assert!(barrier.as_transaction().is_none());
		assert!(barrier.as_bundle().is_none());
		assert!(barrier.transactions().is_empty());
	}

	#[test]
	fn test_apply_tx() {
		// test the checkpoint obtained by application with `Checkpoint::apply`
		let block = BlockContext::<Ethereum>::mocked();
		let root = block.start();

		let tx = test_tx::<Ethereum>(0, 0);
		let checkpoint = root.apply(tx.clone()).unwrap();
		assert_eq!(checkpoint.as_transaction(), Some(&tx));
		assert_eq!(checkpoint.transactions(), std::slice::from_ref(&tx));
		assert!(checkpoint.as_bundle().is_none());
		assert!(!checkpoint.is_barrier());

		// expected mutation result
		let res = tx
			.try_into_executable()
			.unwrap()
			.execute(&block, &root)
			.unwrap();
		assert_eq!(checkpoint.result(), Some(&res));
		assert_eq!(checkpoint.state(), Some(res.state()));
		assert_eq!(checkpoint.transactions(), res.transactions());
		assert_eq!(checkpoint.inner.mutation, Mutation::Executable(res));

		let tx = test_tx::<Ethereum>(0, 1);
		let checkpoint = checkpoint.apply(tx.clone()).unwrap();
		assert_eq!(checkpoint.depth(), 2);

		// This tx is supposed to fail
		let fail_tx = tx;
		let checkpoint_res = checkpoint.apply(fail_tx);
		assert!(checkpoint_res.is_err());
	}

	#[test]
	fn test_apply_bundle() {
		let block = BlockContext::<Ethereum>::mocked();
		let root = block.start();

		let (bundle, txs) = test_bundle::<Ethereum>(0, 0);
		let checkpoint = root.apply(bundle.clone()).unwrap();
		assert_eq!(checkpoint.as_bundle(), Some(&bundle));
		assert_eq!(checkpoint.transactions(), txs.as_slice());
		assert!(checkpoint.as_transaction().is_none());
		assert!(!checkpoint.is_barrier());

		// expected mutation result
		let res = bundle
			.try_into_executable()
			.unwrap()
			.execute(&block, &root)
			.unwrap();
		assert_eq!(checkpoint.result(), Some(&res));
		assert_eq!(checkpoint.state(), Some(res.state()));
		assert_eq!(checkpoint.transactions(), res.transactions());
		assert_eq!(checkpoint.inner.mutation, Mutation::Executable(res));

		let (bundle, _) = test_bundle::<Ethereum>(0, 3);
		let checkpoint = checkpoint.apply(bundle.clone()).unwrap();
		assert_eq!(checkpoint.depth(), 2);

		// This bundle is supposed to fail
		let fail_bundle = bundle;
		let checkpoint_res = checkpoint.apply(fail_bundle);
		assert!(checkpoint_res.is_err());
	}

	#[test]
	fn test_apply_with_tag() {
		let tag = "tagged";
		let block = BlockContext::<Ethereum>::mocked();
		let root = block.start();
		assert!(root.tag().is_none());
		assert!(!root.is_tagged("x"));

		let checkpoint = root.barrier_with_tag(tag);
		assert!(checkpoint.is_tagged(tag));
		assert_eq!(checkpoint.tag(), Some(tag));

		let checkpoint =
			root.apply_with_tag(test_tx::<Ethereum>(0, 0), tag).unwrap();
		assert_eq!(checkpoint.tag(), Some(tag));
	}

	#[test]
	fn test_iter() {
		let block = BlockContext::<Ethereum>::mocked();
		let root = block.start();

		let txs = test_txs::<Ethereum>(0, 0, 10);
		let checkpoints = apply_multiple(root, &txs);

		let latest = checkpoints.last().unwrap();
		let iter = latest.into_iter();

		assert!(
			checkpoints
				.into_iter()
				.rev()
				.zip(iter)
				.all(|(expected_cp, cp)| expected_cp == cp)
		);
	}

	#[test]
	fn test_checkpoint_to_txs() {
		let block = BlockContext::<Ethereum>::mocked();
		let root = block.start();
		let txs = test_txs::<Ethereum>(0, 0, 10);
		let checkpoint = apply_multiple(root, &txs).last().unwrap().to_owned();

		let extracted_txs = Vec::<types::Transaction<Ethereum>>::from(checkpoint);

		assert!(
			txs
				.into_iter()
				.map(|tx| tx.inner().clone())
				.zip(extracted_txs)
				.all(|(expected_tx, tx)| expected_tx == tx)
		);
	}

	#[test]
	fn test_build_payload() {
		let block = BlockContext::<Ethereum>::mocked();
		let provider = block.base_state();

		let root = block.start();
		let txs = test_txs::<Ethereum>(0, 0, 10);
		let checkpoint = apply_multiple(root, &txs).last().unwrap().to_owned();

		let built_payload = checkpoint.build_payload().unwrap();
		let payload = Ethereum::build_payload(checkpoint, provider).unwrap();

		assert_eq!(built_payload.id(), payload.id());
		assert_eq!(built_payload.block(), payload.block());
	}
}
