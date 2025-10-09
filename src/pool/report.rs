//! Order pool status reporting
//!
//! Methods and types in this module are used to report the status of existing
//! orders execution and other information that help the pool decide about what
//! to do with the orders in the pool.

use {
	super::*,
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
	/// and there was an attempt to include it in a payload. Once an order was
	/// proposed once by the pool, it will be removed from the orders list and
	/// will not be proposed again.
	pub fn report_inclusion_attempt(&self, _order_hash: B256, _: PayloadId) {
		// self.remove(&order_hash);
	}

	/// Signals to the order pool that a block has been committed to the chain.
	/// This will remove all orders that had any of their transactions included
	/// in the payload.
	pub fn report_committed_block(&self, block: &SealedBlock<types::Block<P>>) {
		// remove all orders that had any of their transactions included in the
		// payload
		for tx in block.body().transactions() {
			self.remove_any_with(*tx.tx_hash());
		}

		// remove all orders that became permanently ineligible
		// after this block was committed to the chain.
		self.remove_invalidated_orders(block.sealed_header());
	}
}
