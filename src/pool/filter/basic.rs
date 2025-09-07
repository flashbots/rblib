use {
	super::*,
	crate::alloy,
	alloy::{consensus::Transaction, eips::eip1559::MIN_PROTOCOL_BASE_FEE},
};

const MINIMUM_GAS_LIMIT: u64 = 21_000;

/// Filters out orders that are sementically incorrect and violate consensus
/// rules.
#[derive(Default, Debug)]
pub struct BasicFilter;

impl<P: Platform> OrderFilter<P> for BasicFilter {
	fn intrinsic(&self, order: &Order<P>) -> bool {
		for tx in order.transactions() {
			if tx.max_fee_per_gas() < MIN_PROTOCOL_BASE_FEE.into() {
				return false;
			}

			if tx.gas_limit() < MINIMUM_GAS_LIMIT {
				return false;
			}

			if let Some(priority_fee) = tx.max_priority_fee_per_gas() {
				if priority_fee > tx.max_fee_per_gas() {
					return false;
				}
			}
		}

		true
	}
}
