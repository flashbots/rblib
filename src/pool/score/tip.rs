use {
	super::*,
	crate::alloy,
	alloy::consensus::{BlockHeader, Transaction},
};

#[derive(Debug, Clone, Default)]
pub struct PriorityFeeScore;

impl<P: Platform> OrderScore<P> for PriorityFeeScore {
	fn recent_state(
		&self,
		_: &dyn StateProvider,
		header: &SealedHeader<types::Header<P>>,
		order: &Order<P>,
	) -> eyre::Result<Option<Score>> {
		let base_fee = header.base_fee_per_gas().unwrap_or_default();
		Ok(Some(lowest_effective_fee::<P>(base_fee, order).into()))
	}

	fn payload_job(
		&self,
		block: &BlockContext<P>,
		order: &Order<P>,
	) -> eyre::Result<Option<Score>> {
		let base_fee = block.base_fee();
		Ok(Some(lowest_effective_fee::<P>(base_fee, order).into()))
	}
}

fn lowest_effective_fee<P: Platform>(base_fee: u64, order: &Order<P>) -> u128 {
	let mut lowest = u128::MAX;

	for tx in order.transactions() {
		lowest = tx
			.effective_tip_per_gas(base_fee)
			.unwrap_or_default()
			.min(lowest);
	}
	lowest
}
