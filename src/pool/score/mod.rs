use {
	super::*,
	crate::reth,
	core::fmt::Debug,
	derive_more::{Add, AddAssign, Deref, DerefMut, From},
	reth::{primitives::SealedHeader, providers::StateProvider},
};

mod profit;
mod tip;

pub use tip::PriorityFeeScore;

#[derive(
	Clone,
	Copy,
	Default,
	PartialEq,
	Eq,
	PartialOrd,
	Ord,
	Hash,
	Debug,
	From,
	Deref,
	DerefMut,
	Add,
	AddAssign,
)]
pub struct Score(pub u128);

pub trait OrderScore<P: Platform>: Debug + Send + Sync + 'static {
	fn intrinsic(&self, _: &Order<P>) -> Option<Score> {
		None
	}

	fn recent_state(
		&self,
		_: &dyn StateProvider,
		_: &SealedHeader<types::Header<P>>,
		_: &Order<P>,
	) -> eyre::Result<Option<Score>> {
		Ok(None)
	}

	fn payload_job(
		&self,
		_: &BlockContext<P>,
		_: &Order<P>,
	) -> eyre::Result<Option<Score>> {
		Ok(None)
	}

	fn checkpoint(
		&self,
		_: &Checkpoint<P>,
		_: &Order<P>,
	) -> eyre::Result<Option<Score>> {
		Ok(None)
	}
}
