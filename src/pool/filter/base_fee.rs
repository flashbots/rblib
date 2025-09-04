use super::*;

#[derive(Default, Debug)]
pub struct BaseFeeFilter;

impl<P: Platform> OrderFilter<P> for BaseFeeFilter {
	fn block(
		&self,
		_: &BlockContext<P>,
		_: &Order<P>,
	) -> eyre::Result<Eligibility> {
		todo!()
	}

	fn checkpoint(
		&self,
		_: &Checkpoint<P>,
		_: &Order<P>,
	) -> eyre::Result<Eligibility> {
		todo!()
	}
}
