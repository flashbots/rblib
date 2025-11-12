use {
	super::{OrderBy, OrderScore},
	crate::{
		alloy::{consensus::Transaction, primitives::Address},
		prelude::*,
	},
	core::{convert::Infallible, marker::PhantomData},
};

#[derive(Debug, Clone)]
pub struct DestinationAndPriorityFeeScore<P: Platform> {
	destination_address: Address,
	_phantom: PhantomData<P>,
}

impl<P: Platform> Default for DestinationAndPriorityFeeScore<P> {
	fn default() -> Self {
		Self {
			destination_address: Address::ZERO,
			_phantom: PhantomData,
		}
	}
}

impl<P: Platform> DestinationAndPriorityFeeScore<P> {
	pub fn new(destination_address: Address) -> Self {
		Self {
			destination_address,
			_phantom: PhantomData,
		}
	}
}

impl<P: Platform> OrderScore<P> for DestinationAndPriorityFeeScore<P> {
	type Error = Infallible;
	type Score = (u8, u128);

	// Since this implementation of score() returns a tuple, this creates a
	// two-tier priority system which we want: Transactions going to the
	// destination address are always prioritized over transactions that aren't.
	// Within each tier, they're ordered by effective tip.
	fn score(&self, checkpoint: &Checkpoint<P>) -> Result<Self::Score, Self::Error> {
		let all_destination = checkpoint
			.transactions()
			.iter()
			.all(|tx| tx.to() == Some(self.destination_address));
		let tip_sum = CheckpointExt::effective_tip_per_gas(checkpoint);
		Ok((all_destination as u8, tip_sum))
	}
}

pub type OrderByDestinationAndPriorityFee<P> = OrderBy<P, DestinationAndPriorityFeeScore<P>>;
