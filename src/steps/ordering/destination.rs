use {
	super::{OrderBy, OrderScore},
	crate::{alloy::primitives::Address, prelude::*},
	core::{convert::Infallible, marker::PhantomData},
};

#[derive(Debug, Clone, Default)]
pub struct DestinationScore<P: Platform>(PhantomData<P>);

impl<P: Platform> OrderScore<P> for DestinationScore<P> {
	type Error = Infallible;
	type Score = (u8, u128); // (is_destination, tip_sum)

	// TODO: score() is only allowed to take one parameter. We need to find a way
	// to pass the destination address to the score function.
	fn score(
		checkpoint: &Checkpoint<P>,
		destination_address: Address,
	) -> Result<Self::Score, Self::Error> {
		let all_destination = checkpoint
			.transactions()
			.iter()
			.all(|tx| tx.to() == Some(destination_address));
		let tip_sum = checkpoint.effective_tip_per_gas();
		Ok((all_destination as u8, tip_sum))
	}
}

// Alias to use with OrderBy
pub type OrderByDestination<P> = OrderBy<P, DestinationScore<P>>;
