use {
	super::*,
	crate::reth,
	core::fmt::Debug,
	reth::{primitives::SealedHeader, providers::StateProvider},
};

mod balance;
mod base_fee;
mod basic;
mod bundle;
mod max_gas;
mod nonce;

pub use {
	balance::SignerBalanceFilter,
	base_fee::BaseFeeFilter,
	basic::BasicFilter,
	bundle::BundleFilter,
	max_gas::MaxGasPerSenderFilter,
	nonce::NonceFilter,
};

/// Implementations of this trait define the filtering logic for orders in the
/// pool. They decide whether an order should be kept or discarded from the pool
/// and if it should be presented by order streams.
pub trait OrderFilter<P: Platform>: Debug + Sync + Send + 'static {
	/// Decides whether an order is eligible for inclusion using only static
	/// checks without access to the chain state.
	///
	/// This is intended for semantic validations of the order that can be done by
	/// only looking at the order itself, such as checking if max gas is not less
	/// than the intrinsic gas, etc.
	///
	/// Results of intrinsic checks should never change over time, so if an order
	/// is deemed eligible once at this level, it should always be eligible.
	///
	/// Return true if the order passes the intrinsic checks, false otherwise.
	fn intrinsic(&self, _: &Order<P>) -> bool {
		true
	}

	/// Decides whether an order is eligible for inclusion in the order pool.
	///
	/// If this function returns `PermanentlyIneligible` then the given order will
	/// be discarded and never be presented through any stream or iterator. This
	/// test is also used to discard orders at the RPC level before they enter
	/// the pool.
	///
	/// As an input this function receives a reference to the order that is being
	/// tested, a recent committed block header and a state provider at that
	/// block. Don't make too many assumptions about the recency of the header or
	/// state and try to err on the side of caution with marking orders as
	/// permanently ineligible.
	///
	/// This is called periodically when the chain advances to new blocks. At this
	/// stage eligibility may change over time as the chain state changes.
	fn recent_state(
		&self,
		_: &dyn StateProvider,
		_: &SealedHeader<types::Header<P>>,
		_: &Order<P>,
	) -> eyre::Result<Eligibility> {
		Ok(Eligibility::Eligible)
	}

	/// Decides whether an order is eligible for inclusion in a given block under
	/// construction. This test is applied for orders that are proposed by the
	/// `OrdersStream` constructed for a specific block.
	///
	/// If this tests returns `PermanentlyIneligible` for a given order, then it
	/// will also mark the order as permanently ineligible in the global sense,
	/// and it will be discarded from the pool entirely.
	///
	/// This is called once per new order stream instance.
	fn payload_job(
		&self,
		_: &BlockContext<P>,
		_: &Order<P>,
	) -> eyre::Result<Eligibility> {
		Ok(Eligibility::Eligible)
	}

	/// Decides whether an order is eligible for inclusion immediately on top of a
	/// given checkpoint.
	fn checkpoint(
		&self,
		_: &Checkpoint<P>,
		_: &Order<P>,
	) -> eyre::Result<Eligibility> {
		Ok(Eligibility::Eligible)
	}
}

/// Applies all configured filters to an order and returns the lowest
/// eligibility result. It shortcircuits and returns early if any filter
/// returns `PermanentlyIneligible`.
pub(super) struct Filters<'a, P: Platform>(&'a Config<P>);

impl<'a, P: Platform> Filters<'a, P> {
	pub fn of(config: &'a Config<P>) -> Self {
		Self(config)
	}

	pub fn intrinsic(&self, order: &Order<P>) -> bool {
		self.0.filters.iter().all(|f| f.intrinsic(order))
	}

	pub fn recent_state(
		&self,
		state: &dyn StateProvider,
		header: &SealedHeader<types::Header<P>>,
		order: &Order<P>,
	) -> eyre::Result<Eligibility> {
		let mut eligibility = Eligibility::Eligible;

		for filter in &self.0.filters {
			eligibility = eligibility.min(filter.recent_state(state, header, order)?);

			if eligibility == Eligibility::PermanentlyIneligible {
				return Ok(eligibility);
			}
		}

		Ok(eligibility)
	}
}
