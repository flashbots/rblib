use super::*;

#[derive(Debug)]
pub struct MaxGasPerSenderFilter(pub u128);

impl<P: Platform> OrderFilter<P> for MaxGasPerSenderFilter {
	fn checkpoint(
		&self,
		_: &Checkpoint<P>,
		_: &Order<P>,
	) -> eyre::Result<Eligibility> {
		todo!()
	}
}
