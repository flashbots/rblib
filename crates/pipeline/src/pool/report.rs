//! Order pool status reporting
//!
//! Methods and types in this module are used to report the status of existing
//! orders execution and other information that help the pool decide about what
//! to do with the orders in the pool.

use {
	super::*,
	alloy::consensus::BlockHeader,
	reth::{node::builder::BlockBody, primitives::SealedBlock},
	reth_payload_builder::PayloadId,
	tracing::trace,
};

impl<P: Platform> OrderPool<P> {
	/// Invoked when an order was proposed by the pool, but it failed to create a
	/// checkpoint because of an execution error.
	///
	/// Here we will need to decide whether the execution error permanently
	/// invalidates the order and it should be removed from the pool, or if it
	/// can be retried later.
	pub fn report_execution_error(
		&self,
		order_hash: B256,
		error: &ExecutionError<P>,
	) {
		trace!("order marked as invalid: {order_hash} - {error:?}");

		match error {
			// This order is permanently ineligible for inclusion in this and any
			// future payloads. remove it permanently from the pool so it
			// won't be attempted again
			ExecutionError::IneligibleBundle(Eligibility::PermanentlyIneligible) => {
				self.remove(&order_hash);
			}

			ExecutionError::InvalidSignature(_) => {
				// This order is permanently ineligible for inclusion in this and any
				// future payloads. remove it permanently from the pool so it
				// won't be attempted again
				self.remove(&order_hash);
			}

			// TODO: Implement this logic
			_ => {}
		}
	}

	/// Invoked when an order was proposed by the pool through the `best_orders()`
	/// and there was an attempt to include it in a payload built by the given
	/// payload job. Records the considered stage for every transaction of the
	/// order in the lifecycle log; the order stays in the pool.
	pub fn report_inclusion_attempt(
		&self,
		order_hash: B256,
		payload_id: PayloadId,
	) {
		let now = Instant::now();
		self.for_each_transaction_of(order_hash, now, |tx_hash| {
			self
				.inner
				.lifecycle
				.record_considered(tx_hash, payload_id, now);
		});
	}

	/// Invoked when an order proposed by the pool was successfully included in
	/// the payload built by the given payload job.
	pub fn report_inclusion_success(
		&self,
		order_hash: B256,
		payload_id: PayloadId,
	) {
		let now = Instant::now();
		self.for_each_transaction_of(order_hash, now, |tx_hash| {
			self
				.inner
				.lifecycle
				.record_included(tx_hash, payload_id, now);
		});
	}

	/// Signals to the order pool that the transaction pool of the host Reth node
	/// accepted a new transaction at the given moment.
	pub fn report_mempool_transaction(&self, tx_hash: TxHash, received: Instant) {
		self
			.inner
			.lifecycle
			.record_received(tx_hash, TxSource::Mempool, received);
	}

	/// Signals to the order pool that a block has been committed to the chain.
	/// This will remove all orders that had any of their transactions included
	/// in the payload.
	pub fn report_committed_block(&self, block: &SealedBlock<types::Block<P>>) {
		let now = Instant::now();
		let block_number = block.number();
		let block_timestamp = block.timestamp();

		// record the mined stage of all transactions in the block that this
		// pool has seen, before they get evicted from the pool below.
		block.body().transactions().iter().for_each(|tx| {
			self.inner.lifecycle.record_mined(
				*tx.tx_hash(),
				block_number,
				block_timestamp,
				now,
			);
		});

		// remove all orders that had any of their transactions included in the
		// payload
		for tx in block.body().transactions() {
			self.remove_any_with(*tx.tx_hash());
		}

		// remove all orders that became permanently ineligible
		// after this block was committed to the chain.
		self.remove_invalidated_orders(block.sealed_header());

		// forget lifecycles that had no activity for longer than the retention;
		// rate limited so that a burst of committed blocks (sync, reorg) does
		// not run one full prune per block.
		self.inner.lifecycle.prune_if_due(now);
	}

	/// Signals to the order pool that a block was reverted by a chain reorg.
	/// Transactions of that block that this pool has seen are no longer mined
	/// and can be mined again in the new canonical chain.
	pub fn report_reverted_block(&self, block: &SealedBlock<types::Block<P>>) {
		let now = Instant::now();
		let block_number = block.number();

		block.body().transactions().iter().for_each(|tx| {
			self
				.inner
				.lifecycle
				.record_reverted(*tx.tx_hash(), block_number, now);
		});
	}
}

/// Lifecycle bookkeeping helpers
impl<P: Platform> OrderPool<P> {
	/// Invokes `op` with the hash of every transaction of the order with the
	/// given hash. Pipeline events are keyed by order hash and can arrive after
	/// the order was evicted from the pool, so the members are resolved from
	/// the lifecycle log first (which counts as activity of the order record at
	/// `now`, keeping the record of a long-lived order alive), then from the
	/// live orders. For transactions coming from the host node mempool the
	/// order hash is the transaction hash itself, so a hash that neither knows
	/// is treated as a transaction hash.
	///
	/// Residual: after the retention window a late event for a pruned bundle
	/// hash creates a provisional entry keyed by the bundle hash; it ages out
	/// with the retention like any other entry.
	fn for_each_transaction_of(
		&self,
		order_hash: B256,
		now: Instant,
		op: impl FnMut(TxHash),
	) {
		let members = self
			.inner
			.lifecycle
			.members_of(&order_hash, now)
			.or_else(|| {
				self.inner.orders.get(&order_hash).map(|order| {
					order
						.transactions()
						.iter()
						.map(|tx| *tx.tx_hash())
						.collect()
				})
			})
			.unwrap_or_else(|| vec![order_hash]);

		// no `orders` guard is held here
		members.into_iter().for_each(op);
	}
}
