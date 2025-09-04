use super::*;

#[derive(Default, Debug)]
pub struct SignerBalanceFilter;

impl<P: Platform> OrderFilter<P> for SignerBalanceFilter {
	fn global(
		&self,
		_: &dyn StateProvider,
		_: &SealedHeader<types::Header<P>>,
		_: &Order<P>,
	) -> eyre::Result<Eligibility> {
		todo!()
	}

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
