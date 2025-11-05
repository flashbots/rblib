use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::transaction::TxHashRef,
		eips::{Encodable2718, eip2718::WithEncoded},
		network::eip2718::Eip2718Error,
		primitives::{B256, TxHash},
	},
	core::{fmt::Debug, ops::Not},
	reth::{
		ethereum::primitives::{Recovered, crypto::RecoveryError},
		primitives::SealedHeader,
		revm::db::BundleState,
	},
	serde::{Serialize, de::DeserializeOwned},
};

mod flashbots;
pub use flashbots::FlashbotsBundle;

mod ext;
pub use ext::BundleExt;

/// This trait defines a set of transactions that are bundled together and
/// should be processed as a single unit of execution.
///
/// Examples of bundles are objects sent through the [`eth_sendBundle`] JSON-RPC
/// endpoint.
///
/// [`eth_sendBundle`]: https://docs.flashbots.net/flashbots-auction/searchers/advanced/rpc-endpoint#eth_sendbundle
pub trait Bundle<P: Platform>:
	Serialize + DeserializeOwned + Clone + PartialEq + Debug + Send + Sync + 'static
{
	type PostExecutionError: core::error::Error + Send + Sync + 'static;

	/// Access to the transactions that are part of this bundle.
	fn transactions(&self) -> &[Recovered<types::Transaction<P>>];

	/// Returns a new bundle with the exact configuration and metadata as this one
	/// but without the transaction with the given hash. The returned bundle must
	/// maintain the same order of transactions as the original bundle.
	///
	/// The system guarantees that this function will not be called for a
	/// transaction not in the bundle, not optional or not the last remaining in
	/// the bundle.
	#[must_use]
	fn without_transaction(self, tx: TxHash) -> Self;

	/// Checks if the bundle is eligible for inclusion in the block
	/// before executing any of its transactions.
	fn is_eligible(&self, block: &BlockContext<P>) -> Eligibility;

	/// Optional check that allows the bundle to check if it knows for sure that
	/// it will never be eligible for inclusion in any future block that is a
	/// descendant of the given header.
	///
	/// Implementing this method is optional for bundles, but it gives the ability
	/// to filter out bundles at the RPC level before they are sent to the pool.
	fn is_permanently_ineligible(
		&self,
		_: &SealedHeader<types::Header<P>>,
	) -> bool {
		false
	}

	/// Checks if a transaction of the given hash is allowed to not have a
	/// successful execution result for this bundle.
	///
	/// The `tx` is the hash of the transaction to check, that should be part of
	/// this bundle.
	fn is_allowed_to_fail(&self, tx: &TxHash) -> bool;

	/// Checks if a transaction with the given hash may be removed from the
	/// bundle without affecting its validity.
	fn is_optional(&self, tx: &TxHash) -> bool;

	/// An optional check for bundle implementations that have validity
	/// requirements on the resulting state.
	///
	/// For example, an implementation can require a minimal balance for some
	/// accounts after bundle execution. The state passed in this method contains
	/// only entries that were modified or created by executing transactions from
	/// this bundle.
	fn validate_post_execution(
		&self,
		_: &BundleState,
		_: &BlockContext<P>,
	) -> Result<(), Self::PostExecutionError> {
		Ok(())
	}

	/// Calculates the hash of the bundle.
	///
	/// This hash should be unique for each bundle and take into account the
	/// metadata attached to it, so two bundles with the same transactions but
	/// different metadata should have different hashes.
	fn hash(&self) -> B256;

	/// Returns an iterator that yields the bundle's transactions in a format
	/// ready for execution.
	///
	/// By default, this wraps the plain `Recovered` transactions. Implementors
	/// that store pre-encoded bytes can override this to provide the more
	/// efficient `WithEncoded` wrapper.
	fn transactions_encoded(
		&self,
	) -> Box<
		dyn Iterator<Item = WithEncoded<&Recovered<types::Transaction<P>>>> + '_,
	> {
		Box::new(
			self
				.transactions()
				.iter()
				.map(|tx| WithEncoded::new(tx.encoded_2718().into(), tx)),
		)
	}
}

/// The eligibility of a bundle for inclusion in a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
	/// The bundle is eligible for inclusion in this given block. This does not
	/// mean that the bundle will actually be included, but only that static
	/// checks that do not require execution checks have passed.
	///
	/// Bundles in this state, if not included in the block, should be kept in the
	/// pool and retried in future blocks.
	Eligible,

	/// The bundle is not eligible for inclusion in the given block but may
	/// become eligible in the future. An example of this state is when the
	/// bundle has not reached yet the minimum required block number for
	/// inclusion.
	///
	/// Bundles in this state should be kept in the pool and retried in future
	/// blocks.
	TemporarilyIneligible,

	/// The bundle is permanently not eligible for inclusion in this or any
	/// future block. An example of this state is when the current block number
	/// is past the maximum block number of the bundle if the bundle
	/// implementation supports this type of limit.
	///
	/// Bundles in this state should be removed from the pool and not attempted
	/// for inclusion in any future blocks.
	///
	/// Once a bundle returns this state, it should never return any other
	/// eligibility state.
	PermanentlyIneligible,
}

impl Eligibility {
	/// Returns `Some(f())` if the eligibility is `Eligible`, otherwise returns
	/// `None`.
	pub fn then<T, F: FnOnce() -> T>(self, f: F) -> Option<T> {
		matches!(self, Eligibility::Eligible).then(f)
	}

	/// Returns `Some(t)` if the eligibility is `Eligible`, otherwise returns
	/// `None`.
	pub fn then_some<T>(self, t: T) -> Option<T> {
		matches!(self, Eligibility::Eligible).then_some(t)
	}
}

/// This is a quality of life helper that allows users of this api to say:
/// `if bundle.is_eligible(block).into() {.}`, without going into the
/// details of the eligibility.
impl From<Eligibility> for bool {
	fn from(el: Eligibility) -> Self {
		matches!(el, Eligibility::Eligible)
	}
}

/// This is a quality of life helper that allows users of this api to say:
/// `if !bundle.is_eligible(block) {.}`, without going into the details of
/// the ineligibility.
impl Not for Eligibility {
	type Output = bool;

	fn not(self) -> Self::Output {
		!<Eligibility as Into<bool>>::into(self)
	}
}

#[derive(Debug, thiserror::Error)]
pub enum BundleConversionError {
	#[error("EIP-2718 decoding error: {0}")]
	Decoding(#[from] Eip2718Error),

	#[error("Invalid transaction signature: {0}")]
	Signature(#[from] RecoveryError),
}
