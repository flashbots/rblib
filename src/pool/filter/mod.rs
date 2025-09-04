use {super::*, core::fmt::Debug, reth::providers::StateProvider};

mod balance;
mod base_fee;
mod bundle;
mod max_gas;
mod nonce;

pub use {
	balance::SignerBalanceFilter,
	base_fee::BaseFeeFilter,
	bundle::BundleFilter,
	max_gas::MaxGasPerSenderFilter,
	nonce::NonceFilter,
};

/// Implementations of this trait define the filtering logic for orders in the
/// pool. They decide whether an order should be kept or discarded from the pool
/// and if it should be presented by order streams.
pub trait OrderFilter<P: Platform>: Debug + Sync + Send + 'static {
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
	/// This is called periodically when the chain advances to new blocks.
	fn global(
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
	fn block(
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
