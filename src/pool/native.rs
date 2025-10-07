//! Reth native `TransactionPool`

use {
	crate::{
		alloy::{
			eips::{
				eip4844::{BlobAndProofV1, BlobAndProofV2},
				eip7594::BlobTransactionSidecarVariant,
			},
			primitives::{Address, B256, TxHash},
		},
		prelude::*,
		reth::{
			network::types::HandleMempoolData,
			primitives::Recovered,
			transaction_pool::{TransactionPool as RethTransactionPoolTrait, *},
		},
	},
	core::fmt::Debug,
	futures::FutureExt,
	std::{collections::HashSet, future::Future, pin::Pin, sync::Arc},
	tokio::sync::mpsc::Receiver,
};

/// A type that gives access to the transaction pool facilities inside a
/// pipeline step without needing to know the concrete type of the pool.
///
/// It is unfortunate that Reth's `TransactionPool` trait is not dyn-safe,
/// because it inherits from `Clone`, which forces us to wrap it here.
pub(crate) struct NativeTransactionPool<P: Platform> {
	vtable: TransactionPoolVTable<P>,
}

impl<P: Platform> Clone for NativeTransactionPool<P> {
	fn clone(&self) -> Self {
		let vtable_clone = self.vtable.clone();
		let cloned = (self.vtable.clone)(self.vtable.self_ptr, vtable_clone);
		Self { vtable: cloned }
	}
}

impl<P: Platform> NativeTransactionPool<P> {
	pub(crate) fn new<Pool: traits::PoolBounds<P>>(pool: Arc<Pool>) -> Self {
		let vtable = TransactionPoolVTable::new(pool);
		Self { vtable }
	}
}

impl<P: Platform> Debug for NativeTransactionPool<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(f, "TransactionPool ({:?})", &self.pool_size())
	}
}

unsafe impl<P: Platform> Send for NativeTransactionPool<P> {}
unsafe impl<P: Platform> Sync for NativeTransactionPool<P> {}

/// V-Table struct that captures all methods from the `TransactionPool` trait
#[allow(clippy::type_complexity)]
#[derive(Clone)]
struct TransactionPoolVTable<P: Platform> {
	self_ptr: *const u8,
	clone: fn(*const u8, Self) -> Self,
	pool_size: fn(*const u8) -> PoolSize,
	block_info: fn(*const u8) -> BlockInfo,
	add_transaction: fn(
		*const u8,
		TransactionOrigin,
		P::PooledTransaction,
	) -> Pin<
		Box<dyn Future<Output = PoolResult<AddedTransactionOutcome>> + Send>,
	>,
	add_transaction_and_subscribe: fn(
		*const u8,
		TransactionOrigin,
		P::PooledTransaction,
	) -> Pin<
		Box<dyn Future<Output = PoolResult<TransactionEvents>> + Send>,
	>,
	add_transactions: fn(
		*const u8,
		TransactionOrigin,
		Vec<P::PooledTransaction>,
	) -> Pin<
		Box<dyn Future<Output = Vec<PoolResult<AddedTransactionOutcome>>> + Send>,
	>,
	add_transactions_with_origins: fn(
		*const u8,
		Vec<(TransactionOrigin, P::PooledTransaction)>,
	) -> Pin<
		Box<dyn Future<Output = Vec<PoolResult<AddedTransactionOutcome>>> + Send>,
	>,
	transaction_event_listener:
		fn(*const u8, TxHash) -> Option<TransactionEvents>,
	all_transactions_event_listener:
		fn(*const u8) -> AllTransactionsEvents<P::PooledTransaction>,
	pending_transactions_listener_for:
		fn(*const u8, TransactionListenerKind) -> Receiver<TxHash>,
	blob_transaction_sidecars_listener: fn(*const u8) -> Receiver<NewBlobSidecar>,
	new_transactions_listener_for:
		fn(
			*const u8,
			TransactionListenerKind,
		) -> Receiver<NewTransactionEvent<P::PooledTransaction>>,
	pooled_transaction_hashes: fn(*const u8) -> Vec<TxHash>,
	pooled_transaction_hashes_max: fn(*const u8, usize) -> Vec<TxHash>,
	pooled_transactions:
		fn(*const u8) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	pooled_transactions_max:
		fn(
			*const u8,
			usize,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_pooled_transaction_elements: fn(
		*const u8,
		Vec<TxHash>,
		GetPooledTransactionLimit,
	) -> Vec<
		<P::PooledTransaction as reth_transaction_pool::PoolTransaction>::Pooled,
	>,
	get_pooled_transaction_element: fn(
		*const u8,
		TxHash,
	) -> Option<
		Recovered<
			<P::PooledTransaction as reth_transaction_pool::PoolTransaction>::Pooled,
		>,
	>,
	best_transactions: fn(
		*const u8,
	) -> Box<
		dyn BestTransactions<
			Item = Arc<ValidPoolTransaction<P::PooledTransaction>>,
		>,
	>,
	best_transactions_with_attributes: fn(
		*const u8,
		BestTransactionsAttributes,
	) -> Box<
		dyn BestTransactions<
			Item = Arc<ValidPoolTransaction<P::PooledTransaction>>,
		>,
	>,
	pending_transactions:
		fn(*const u8) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	pending_transactions_max:
		fn(
			*const u8,
			usize,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	queued_transactions:
		fn(*const u8) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	all_transactions: fn(*const u8) -> AllPoolTransactions<P::PooledTransaction>,
	all_transactions_hashes: fn(*const u8) -> Vec<TxHash>,
	remove_transactions:
		fn(
			*const u8,
			Vec<TxHash>,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	remove_transactions_and_descendants:
		fn(
			*const u8,
			Vec<TxHash>,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	remove_transactions_by_sender:
		fn(
			*const u8,
			Address,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	contains: fn(*const u8, &TxHash) -> bool,
	get: fn(
		*const u8,
		&TxHash,
	) -> Option<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_all: fn(
		*const u8,
		Vec<TxHash>,
	) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	on_propagated: fn(*const u8, PropagatedTransactions),
	get_transactions_by_sender:
		fn(
			*const u8,
			Address,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_pending_transactions_by_sender:
		fn(
			*const u8,
			Address,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_queued_transactions_by_sender:
		fn(
			*const u8,
			Address,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_highest_transaction_by_sender:
		fn(
			*const u8,
			Address,
		) -> Option<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_highest_consecutive_transaction_by_sender:
		fn(
			*const u8,
			Address,
			u64,
		) -> Option<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_transaction_by_sender_and_nonce:
		fn(
			*const u8,
			Address,
			u64,
		) -> Option<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_transactions_by_origin:
		fn(
			*const u8,
			TransactionOrigin,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,
	get_pending_transactions_by_origin:
		fn(
			*const u8,
			TransactionOrigin,
		) -> Vec<Arc<ValidPoolTransaction<P::PooledTransaction>>>,

	unique_senders: fn(*const u8) -> HashSet<Address>,
	get_blob:
		fn(
			*const u8,
			TxHash,
		) -> Result<Option<Arc<BlobTransactionSidecarVariant>>, BlobStoreError>,
	get_all_blobs: fn(
		*const u8,
		Vec<TxHash>,
	) -> Result<
		Vec<(TxHash, Arc<BlobTransactionSidecarVariant>)>,
		BlobStoreError,
	>,
	get_all_blobs_exact:
		fn(
			*const u8,
			Vec<TxHash>,
		) -> Result<Vec<Arc<BlobTransactionSidecarVariant>>, BlobStoreError>,
	get_blobs_for_versioned_hashes_v1:
		fn(
			*const u8,
			&[B256],
		) -> Result<Vec<Option<BlobAndProofV1>>, BlobStoreError>,
	get_blobs_for_versioned_hashes_v2:
		fn(
			*const u8,
			&[B256],
		) -> Result<Option<Vec<BlobAndProofV2>>, BlobStoreError>,
	pending_and_queued_txn_count: fn(*const u8) -> (usize, usize),
}

impl<P: Platform> TransactionPoolVTable<P> {
	/// Creates a new vtable from a `TransactionPool` implementation
	#[allow(clippy::too_many_lines)]
	pub(crate) fn new<Pool: traits::PoolBounds<P>>(pool: Pool) -> Self {
		let self_ptr = Box::into_raw(Box::new(pool)) as *const u8;

		Self {
			self_ptr,
			clone: |self_ptr: *const u8, other: Self| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				let cloned_self_ptr =
					Box::into_raw(Box::new(pool.clone())) as *const u8;

				Self {
					self_ptr: cloned_self_ptr,
					..other
				}
			},
			pool_size: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pool_size()
			},
			block_info: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.block_info()
			},
			add_transaction: |self_ptr: *const u8, origin, tx| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.add_transaction(origin, tx).boxed()
			},
			add_transaction_and_subscribe: |self_ptr: *const u8, origin, tx| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.add_transaction_and_subscribe(origin, tx).boxed()
			},
			add_transactions: |self_ptr: *const u8, origin, txs| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.add_transactions(origin, txs).boxed()
			},
			add_transactions_with_origins: |self_ptr: *const u8, txs| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.add_transactions_with_origins(txs).boxed()
			},
			transaction_event_listener: |self_ptr: *const u8, tx_hash| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.transaction_event_listener(tx_hash)
			},
			all_transactions_event_listener: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.all_transactions_event_listener()
			},
			all_transactions_hashes: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.all_transaction_hashes()
			},
			pending_transactions_listener_for: |self_ptr: *const u8, kind| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pending_transactions_listener_for(kind)
			},
			blob_transaction_sidecars_listener: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.blob_transaction_sidecars_listener()
			},
			new_transactions_listener_for: |self_ptr: *const u8, kind| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.new_transactions_listener_for(kind)
			},
			pooled_transaction_hashes: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pooled_transaction_hashes()
			},
			pooled_transaction_hashes_max: |self_ptr: *const u8, max| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pooled_transaction_hashes_max(max)
			},
			pooled_transactions: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pooled_transactions()
			},
			pooled_transactions_max: |self_ptr: *const u8, max| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pooled_transactions_max(max)
			},
			get_pooled_transaction_elements:
				|self_ptr: *const u8, tx_hashes, limit| {
					let pool = unsafe { &*self_ptr.cast::<Pool>() };
					pool.get_pooled_transaction_elements(tx_hashes, limit)
				},
			get_pooled_transaction_element: |self_ptr: *const u8, tx_hash| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_pooled_transaction_element(tx_hash)
			},
			best_transactions: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.best_transactions()
			},
			best_transactions_with_attributes: |self_ptr: *const u8, attributes| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.best_transactions_with_attributes(attributes)
			},
			pending_transactions: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pending_transactions()
			},
			pending_transactions_max: |self_ptr: *const u8, max| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pending_transactions_max(max)
			},
			queued_transactions: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.queued_transactions()
			},
			all_transactions: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.all_transactions()
			},
			remove_transactions: |self_ptr: *const u8, hashes| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.remove_transactions(hashes)
			},
			remove_transactions_and_descendants: |self_ptr: *const u8, hashes| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.remove_transactions_and_descendants(hashes)
			},
			remove_transactions_by_sender: |self_ptr: *const u8, sender| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.remove_transactions_by_sender(sender)
			},
			contains: |self_ptr: *const u8, tx_hash| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.contains(tx_hash)
			},
			get: |self_ptr: *const u8, tx_hash| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get(tx_hash)
			},
			get_all: |self_ptr: *const u8, txs| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_all(txs)
			},
			on_propagated: |self_ptr: *const u8, txs| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.on_propagated(txs);
			},
			get_transactions_by_sender: |self_ptr: *const u8, sender| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_transactions_by_sender(sender)
			},
			get_pending_transactions_by_sender: |self_ptr: *const u8, sender| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_pending_transactions_by_sender(sender)
			},
			get_queued_transactions_by_sender: |self_ptr: *const u8, sender| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_queued_transactions_by_sender(sender)
			},
			get_highest_transaction_by_sender: |self_ptr: *const u8, sender| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_highest_transaction_by_sender(sender)
			},
			get_highest_consecutive_transaction_by_sender:
				|self_ptr: *const u8, sender, nonce| {
					let pool = unsafe { &*self_ptr.cast::<Pool>() };
					pool.get_highest_consecutive_transaction_by_sender(sender, nonce)
				},
			get_transaction_by_sender_and_nonce:
				|self_ptr: *const u8, sender, nonce| {
					let pool = unsafe { &*self_ptr.cast::<Pool>() };
					pool.get_transaction_by_sender_and_nonce(sender, nonce)
				},
			get_transactions_by_origin: |self_ptr: *const u8, origin| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_transactions_by_origin(origin)
			},
			get_pending_transactions_by_origin: |self_ptr: *const u8, origin| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_pending_transactions_by_origin(origin)
			},
			unique_senders: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.unique_senders()
			},
			get_blob: |self_ptr: *const u8, tx_hash| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_blob(tx_hash)
			},
			get_all_blobs: |self_ptr: *const u8, tx_hashes| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_all_blobs(tx_hashes)
			},
			get_all_blobs_exact: |self_ptr: *const u8, tx_hashes| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.get_all_blobs_exact(tx_hashes)
			},
			get_blobs_for_versioned_hashes_v1:
				|self_ptr: *const u8, versioned_hashes| {
					let pool = unsafe { &*self_ptr.cast::<Pool>() };
					pool.get_blobs_for_versioned_hashes_v1(versioned_hashes)
				},
			get_blobs_for_versioned_hashes_v2:
				|self_ptr: *const u8, versioned_hashes| {
					let pool = unsafe { &*self_ptr.cast::<Pool>() };
					pool.get_blobs_for_versioned_hashes_v2(versioned_hashes)
				},
			pending_and_queued_txn_count: |self_ptr: *const u8| {
				let pool = unsafe { &*self_ptr.cast::<Pool>() };
				pool.pending_and_queued_txn_count()
			},
		}
	}
}

impl<P: Platform> Drop for TransactionPoolVTable<P> {
	fn drop(&mut self) {
		// SAFETY: We are the only owner of the pool, so it is safe to drop it.
		let _ = unsafe { Box::from_raw(self.self_ptr as *mut P) };
	}
}

impl<P: Platform> RethTransactionPoolTrait for NativeTransactionPool<P> {
	/// The transaction type of the pool
	type Transaction = P::PooledTransaction;

	/// Returns stats about the pool and all sub-pools.
	fn pool_size(&self) -> PoolSize {
		(self.vtable.pool_size)(self.vtable.self_ptr)
	}

	/// Returns the block the pool is currently tracking.
	///
	/// This tracks the block that the pool has last seen.
	fn block_info(&self) -> BlockInfo {
		(self.vtable.block_info)(self.vtable.self_ptr)
	}

	/// Adds an _unvalidated_ transaction into the pool and subscribe to state
	/// changes.
	///
	/// This is the same as [`TransactionPool::add_transaction`] but returns an
	/// event stream for the
	/// given transaction.
	///
	/// Consumer: Custom
	fn add_transaction_and_subscribe(
		&self,
		origin: TransactionOrigin,
		transaction: Self::Transaction,
	) -> impl Future<Output = PoolResult<TransactionEvents>> + Send {
		(self.vtable.add_transaction_and_subscribe)(
			self.vtable.self_ptr,
			origin,
			transaction,
		)
	}

	/// Adds an _unvalidated_ transaction into the pool.
	///
	/// Consumer: RPC
	fn add_transaction(
		&self,
		origin: TransactionOrigin,
		transaction: Self::Transaction,
	) -> impl Future<Output = PoolResult<AddedTransactionOutcome>> + Send {
		(self.vtable.add_transaction)(self.vtable.self_ptr, origin, transaction)
	}

	/// Adds the given _unvalidated_ transactions into the pool.
	///
	/// All transactions will use the same `origin`.
	///
	/// Returns a list of results.
	///
	/// Consumer: RPC
	fn add_transactions(
		&self,
		origin: TransactionOrigin,
		transactions: Vec<Self::Transaction>,
	) -> impl Future<Output = Vec<PoolResult<AddedTransactionOutcome>>> + Send {
		(self.vtable.add_transactions)(self.vtable.self_ptr, origin, transactions)
	}

	/// Adds multiple _unvalidated_ transactions with individual origins.
	///
	/// Each transaction can have its own [`TransactionOrigin`].
	///
	/// Consumer: RPC
	fn add_transactions_with_origins(
		&self,
		transactions: Vec<(TransactionOrigin, Self::Transaction)>,
	) -> impl Future<Output = Vec<PoolResult<AddedTransactionOutcome>>> + Send {
		(self.vtable.add_transactions_with_origins)(
			self.vtable.self_ptr,
			transactions,
		)
	}

	/// Returns a new transaction change event stream for the given transaction.
	///
	/// Returns `None` if the transaction is not in the pool.
	fn transaction_event_listener(
		&self,
		tx_hash: TxHash,
	) -> Option<TransactionEvents> {
		(self.vtable.transaction_event_listener)(self.vtable.self_ptr, tx_hash)
	}

	/// Returns a new transaction change event stream for _all_ transactions in
	/// the pool.
	fn all_transactions_event_listener(
		&self,
	) -> AllTransactionsEvents<Self::Transaction> {
		(self.vtable.all_transactions_event_listener)(self.vtable.self_ptr)
	}

	/// Returns a new [Receiver] that yields transactions hashes for new
	/// __pending__ transactions
	/// inserted into the pending pool depending on the given
	/// [`TransactionListenerKind`] argument.
	fn pending_transactions_listener_for(
		&self,
		kind: TransactionListenerKind,
	) -> Receiver<TxHash> {
		(self.vtable.pending_transactions_listener_for)(self.vtable.self_ptr, kind)
	}

	/// Returns a new [Receiver] that yields blob "sidecars" (blobs w/ assoc. kzg
	/// commitments/proofs) for eip-4844 transactions inserted into the pool
	fn blob_transaction_sidecars_listener(&self) -> Receiver<NewBlobSidecar> {
		(self.vtable.blob_transaction_sidecars_listener)(self.vtable.self_ptr)
	}

	/// Returns a new stream that yields new valid transactions added to the pool
	/// depending on the given [`TransactionListenerKind`] argument.
	fn new_transactions_listener_for(
		&self,
		kind: TransactionListenerKind,
	) -> Receiver<NewTransactionEvent<Self::Transaction>> {
		(self.vtable.new_transactions_listener_for)(self.vtable.self_ptr, kind)
	}

	/// Returns the _hashes_ of all transactions in the pool.
	///
	/// Note: This returns a `Vec` but should guarantee that all hashes are
	/// unique.
	///
	/// Consumer: P2P
	fn pooled_transaction_hashes(&self) -> Vec<TxHash> {
		(self.vtable.pooled_transaction_hashes)(self.vtable.self_ptr)
	}

	/// Returns only the first `max` hashes of transactions in the pool.
	///
	/// Consumer: P2P
	fn pooled_transaction_hashes_max(&self, max: usize) -> Vec<TxHash> {
		(self.vtable.pooled_transaction_hashes_max)(self.vtable.self_ptr, max)
	}

	/// Returns the _full_ transaction objects all transactions in the pool.
	///
	/// This is intended to be used by the network for the initial exchange of
	/// pooled transaction
	/// _hashes_
	///
	/// Note: This returns a `Vec` but should guarantee that all transactions are
	/// unique.
	///
	/// Caution: In case of blob transactions, this does not include the sidecar.
	///
	/// Consumer: P2P
	fn pooled_transactions(
		&self,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.pooled_transactions)(self.vtable.self_ptr)
	}

	/// Returns only the first `max` transactions in the pool.
	///
	/// Consumer: P2P
	fn pooled_transactions_max(
		&self,
		max: usize,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.pooled_transactions_max)(self.vtable.self_ptr, max)
	}

	/// Returns converted [`PooledTransactionVariant`] for the given transaction
	/// hashes.
	///
	/// This adheres to the expected behavior of
	/// [`GetPooledTransactions`](https://github.com/ethereum/devp2p/blob/master/caps/eth.md#getpooledtransactions-0x09):
	///
	/// The transactions must be in same order as in the request, but it is OK to
	/// skip transactions
	/// which are not available.
	///
	/// If the transaction is a blob transaction, the sidecar will be included.
	///
	/// Consumer: P2P
	fn get_pooled_transaction_elements(
		&self,
		tx_hashes: Vec<TxHash>,
		limit: GetPooledTransactionLimit,
	) -> Vec<<Self::Transaction as PoolTransaction>::Pooled> {
		(self.vtable.get_pooled_transaction_elements)(
			self.vtable.self_ptr,
			tx_hashes,
			limit,
		)
	}

	/// Returns the pooled transaction variant for the given transaction hash.
	///
	/// This adheres to the expected behavior of
	/// [`GetPooledTransactions`](https://github.com/ethereum/devp2p/blob/master/caps/eth.md#getpooledtransactions-0x09):
	///
	/// If the transaction is a blob transaction, the sidecar will be included.
	///
	/// It is expected that this variant represents the valid p2p format for full
	/// transactions.
	/// E.g. for EIP-4844 transactions this is the consensus transaction format
	/// with the blob
	/// sidecar.
	///
	/// Consumer: P2P
	fn get_pooled_transaction_element(
		&self,
		tx_hash: TxHash,
	) -> Option<Recovered<<Self::Transaction as PoolTransaction>::Pooled>> {
		(self.vtable.get_pooled_transaction_element)(self.vtable.self_ptr, tx_hash)
	}

	/// Returns an iterator that yields transactions that are ready for block
	/// production.
	///
	/// Consumer: Block production
	fn best_transactions(
		&self,
	) -> Box<
		dyn BestTransactions<Item = Arc<ValidPoolTransaction<Self::Transaction>>>,
	> {
		(self.vtable.best_transactions)(self.vtable.self_ptr)
	}

	/// Returns an iterator that yields transactions that are ready for block
	/// production with the
	/// given base fee and optional blob fee attributes.
	///
	/// Consumer: Block production
	fn best_transactions_with_attributes(
		&self,
		best_transactions_attributes: BestTransactionsAttributes,
	) -> Box<
		dyn BestTransactions<Item = Arc<ValidPoolTransaction<Self::Transaction>>>,
	> {
		(self.vtable.best_transactions_with_attributes)(
			self.vtable.self_ptr,
			best_transactions_attributes,
		)
	}

	/// Returns all transactions that can be included in the next block.
	///
	/// This is primarily used for the `txpool_` RPC namespace:
	/// <https://geth.ethereum.org/docs/interacting-with-geth/rpc/ns-txpool> which distinguishes
	/// between `pending` and `queued` transactions, where `pending` are
	/// transactions ready for
	/// inclusion in the next block and `queued` are transactions that are ready
	/// for inclusion in
	/// future blocks.
	///
	/// Consumer: RPC
	fn pending_transactions(
		&self,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.pending_transactions)(self.vtable.self_ptr)
	}

	/// Returns first `max` transactions that can be included in the next block.
	/// See <https://github.com/paradigmxyz/reth/issues/12767#issuecomment-2493223579>
	///
	/// Consumer: Block production
	fn pending_transactions_max(
		&self,
		max: usize,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.pending_transactions_max)(self.vtable.self_ptr, max)
	}

	/// Returns all transactions that can be included in _future_ blocks.
	///
	/// This and [`Self::pending_transactions`] are mutually exclusive.
	///
	/// Consumer: RPC
	fn queued_transactions(
		&self,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.queued_transactions)(self.vtable.self_ptr)
	}

	/// Returns all transactions that are currently in the pool grouped by whether
	/// they are ready for inclusion in the next block or not.
	///
	/// This is primarily used for the `txpool_` namespace: <https://geth.ethereum.org/docs/interacting-with-geth/rpc/ns-txpool>
	///
	/// Consumer: RPC
	fn all_transactions(&self) -> AllPoolTransactions<Self::Transaction> {
		(self.vtable.all_transactions)(self.vtable.self_ptr)
	}

	/// Returns the _hashes_ of all transactions regardless of whether they can be
	/// propagated or not.
	///
	/// Unlike [`Self::pooled_transaction_hashes`] this doesn't consider whether
	/// the transaction can be propagated or not.
	///
	/// Note: This returns a `Vec` but should guarantee that all hashes are
	/// unique.
	///
	/// Consumer: Utility
	fn all_transaction_hashes(&self) -> Vec<TxHash> {
		(self.vtable.all_transactions_hashes)(self.vtable.self_ptr)
	}

	/// Removes all transactions corresponding to the given hashes.
	///
	/// Note: This removes the transactions as if they got discarded (_not_
	/// mined).
	///
	/// Consumer: Utility
	fn remove_transactions(
		&self,
		hashes: Vec<TxHash>,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.remove_transactions)(self.vtable.self_ptr, hashes)
	}

	/// Removes all transactions corresponding to the given hashes.
	///
	/// Also removes all _dependent_ transactions.
	///
	/// Consumer: Utility
	fn remove_transactions_and_descendants(
		&self,
		hashes: Vec<TxHash>,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.remove_transactions_and_descendants)(
			self.vtable.self_ptr,
			hashes,
		)
	}

	/// Removes all transactions from the given sender
	///
	/// Consumer: Utility
	fn remove_transactions_by_sender(
		&self,
		sender: Address,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.remove_transactions_by_sender)(self.vtable.self_ptr, sender)
	}

	/// Retains only those hashes that are unknown to the pool.
	/// In other words, removes all transactions from the given set that are
	/// currently present in
	/// the pool. Returns hashes already known to the pool.
	///
	/// Consumer: P2P
	fn retain_unknown<A>(&self, _: &mut A)
	where
		A: HandleMempoolData,
	{
		unimplemented!("retain_unknown is not implemented for TransactionPool");
	}

	/// Returns if the transaction for the given hash is already included in this
	/// pool.
	fn contains(&self, tx_hash: &TxHash) -> bool {
		(self.vtable.contains)(self.vtable.self_ptr, tx_hash)
	}

	/// Returns the transaction for the given hash.
	fn get(
		&self,
		tx_hash: &TxHash,
	) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get)(self.vtable.self_ptr, tx_hash)
	}

	/// Returns all transactions objects for the given hashes.
	///
	/// Caution: This in case of blob transactions, this does not include the
	/// sidecar.
	fn get_all(
		&self,
		txs: Vec<TxHash>,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_all)(self.vtable.self_ptr, txs)
	}

	/// Notify the pool about transactions that are propagated to peers.
	///
	/// Consumer: P2P
	fn on_propagated(&self, txs: PropagatedTransactions) {
		(self.vtable.on_propagated)(self.vtable.self_ptr, txs);
	}

	/// Returns all transactions sent by a given user
	fn get_transactions_by_sender(
		&self,
		sender: Address,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_transactions_by_sender)(self.vtable.self_ptr, sender)
	}

	/// Returns all pending transactions filtered by predicate
	fn get_pending_transactions_with_predicate(
		&self,
		_: impl FnMut(&ValidPoolTransaction<Self::Transaction>) -> bool,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		unimplemented!(
			"get_pending_transactions_with_predicate is not implemented"
		);
	}

	/// Returns all pending transactions sent by a given user
	fn get_pending_transactions_by_sender(
		&self,
		sender: Address,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_pending_transactions_by_sender)(
			self.vtable.self_ptr,
			sender,
		)
	}

	/// Returns all queued transactions sent by a given user
	fn get_queued_transactions_by_sender(
		&self,
		sender: Address,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_queued_transactions_by_sender)(
			self.vtable.self_ptr,
			sender,
		)
	}

	/// Returns the highest transaction sent by a given user
	fn get_highest_transaction_by_sender(
		&self,
		sender: Address,
	) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_highest_transaction_by_sender)(
			self.vtable.self_ptr,
			sender,
		)
	}

	/// Returns the transaction with the highest nonce that is executable given
	/// the on chain nonce.
	/// In other words the highest non nonce gapped transaction.
	///
	/// Note: The next pending pooled transaction must have the on chain nonce.
	///
	/// For example, for a given on chain nonce of `5`, the next transaction must
	/// have that nonce.
	/// If the pool contains txs `[5,6,7]` this returns tx `7`.
	/// If the pool contains txs `[6,7]` this returns `None` because the next
	/// valid nonce (5) is
	/// missing, which means txs `[6,7]` are nonce gapped.
	fn get_highest_consecutive_transaction_by_sender(
		&self,
		sender: Address,
		on_chain_nonce: u64,
	) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_highest_consecutive_transaction_by_sender)(
			self.vtable.self_ptr,
			sender,
			on_chain_nonce,
		)
	}

	/// Returns a transaction sent by a given user and a nonce
	fn get_transaction_by_sender_and_nonce(
		&self,
		sender: Address,
		nonce: u64,
	) -> Option<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_transaction_by_sender_and_nonce)(
			self.vtable.self_ptr,
			sender,
			nonce,
		)
	}

	/// Returns all transactions that where submitted with the given
	/// [`TransactionOrigin`]
	fn get_transactions_by_origin(
		&self,
		origin: TransactionOrigin,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_transactions_by_origin)(self.vtable.self_ptr, origin)
	}

	/// Returns all pending transactions filtered by [`TransactionOrigin`]
	fn get_pending_transactions_by_origin(
		&self,
		origin: TransactionOrigin,
	) -> Vec<Arc<ValidPoolTransaction<Self::Transaction>>> {
		(self.vtable.get_pending_transactions_by_origin)(
			self.vtable.self_ptr,
			origin,
		)
	}

	/// Returns a set of all senders of transactions in the pool
	fn unique_senders(&self) -> HashSet<Address> {
		(self.vtable.unique_senders)(self.vtable.self_ptr)
	}

	/// Returns the [`BlobTransactionSidecarVariant`] for the given transaction
	/// hash if it exists in
	/// the blob store.
	fn get_blob(
		&self,
		tx_hash: TxHash,
	) -> Result<Option<Arc<BlobTransactionSidecarVariant>>, BlobStoreError> {
		(self.vtable.get_blob)(self.vtable.self_ptr, tx_hash)
	}

	/// Returns all [`BlobTransactionSidecarVariant`] for the given transaction
	/// hashes if they
	/// exists in the blob store.
	///
	/// This only returns the blobs that were found in the store.
	/// If there's no blob it will not be returned.
	fn get_all_blobs(
		&self,
		tx_hashes: Vec<TxHash>,
	) -> Result<Vec<(TxHash, Arc<BlobTransactionSidecarVariant>)>, BlobStoreError>
	{
		(self.vtable.get_all_blobs)(self.vtable.self_ptr, tx_hashes)
	}

	/// Returns the exact [`BlobTransactionSidecarVariant`] for the given
	/// transaction hashes in the
	/// order they were requested.
	///
	/// Returns an error if any of the blobs are not found in the blob store.
	fn get_all_blobs_exact(
		&self,
		tx_hashes: Vec<TxHash>,
	) -> Result<Vec<Arc<BlobTransactionSidecarVariant>>, BlobStoreError> {
		(self.vtable.get_all_blobs_exact)(self.vtable.self_ptr, tx_hashes)
	}

	/// Return the [`BlobAndProofV1`]s for a list of blob versioned hashes.
	fn get_blobs_for_versioned_hashes_v1(
		&self,
		versioned_hashes: &[B256],
	) -> Result<Vec<Option<BlobAndProofV1>>, BlobStoreError> {
		(self.vtable.get_blobs_for_versioned_hashes_v1)(
			self.vtable.self_ptr,
			versioned_hashes,
		)
	}

	/// Return the [`BlobAndProofV2`]s for a list of blob versioned hashes.
	/// Blobs and proofs are returned only if they are present for _all_ of the
	/// requested versioned
	/// hashes.
	fn get_blobs_for_versioned_hashes_v2(
		&self,
		versioned_hashes: &[B256],
	) -> Result<Option<Vec<BlobAndProofV2>>, BlobStoreError> {
		(self.vtable.get_blobs_for_versioned_hashes_v2)(
			self.vtable.self_ptr,
			versioned_hashes,
		)
	}

	/// Returns the number of transactions that are ready for inclusion in the
	/// next block and the number of transactions that are ready for inclusion in
	/// future blocks: `(pending, queued)`.
	fn pending_and_queued_txn_count(&self) -> (usize, usize) {
		(self.vtable.pending_and_queued_txn_count)(self.vtable.self_ptr)
	}
}
