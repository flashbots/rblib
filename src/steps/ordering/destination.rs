use {
	super::{OrderBy, OrderScore},
	crate::{
		alloy::{consensus::Transaction, primitives::Address},
		prelude::*,
	},
	core::{convert::Infallible, marker::PhantomData},
};

#[derive(Debug, Clone, Default)]
pub struct DestinationScore<P: Platform>(PhantomData<P>);

impl<P: Platform> OrderScore<P> for DestinationScore<P> {
	type Error = Infallible;
	type Score = (u8, u128);

	// TODO: score() is only allowed to take one parameter. We need to find a way
	// to pass the destination address to the score function.

	// Since this implementation of score() returns a tuple, this creates a
	// two-tier priority system which we want: Transactions going to the
	// destination address (Address::ZERO) are always prioritized over others
	// Within each tier, they're ordered by effective tip
	fn score(checkpoint: &Checkpoint<P>) -> Result<Self::Score, Self::Error> {
		let all_destination = checkpoint
			.transactions()
			.iter()
			.all(|tx| tx.to() == Some(Address::ZERO));
		let tip_sum = CheckpointExt::effective_tip_per_gas(checkpoint);
		Ok((all_destination as u8, tip_sum))
	}
}

pub type OrderByDestination<P> = OrderBy<P, DestinationScore<P>>;
