use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::BlockHeader,
		eips::{Decodable2718, Encodable2718},
		network::eip2718::Eip2718Error,
		primitives::{B256, Keccak256, TxHash},
	},
	core::{
		convert::Infallible,
		fmt::Debug,
		ops::{Deref, DerefMut, Not},
	},
	reth::{
		ethereum::primitives::{
			Recovered,
			SignedTransaction,
			SignerRecoverable,
			crypto::RecoveryError,
		},
		primitives::SealedHeader,
		revm::db::BundleState,
		rpc::types::mev::EthSendBundle,
	},
	serde::{
		Deserialize,
		Deserializer,
		Serialize,
		Serializer,
		de::DeserializeOwned,
	},
};

/// This trait defines a set of transactions that are bundled together and
/// should be processed as a single unit of execution.
///
/// Examples of bundles are objects sent through the `eth_sendBundle` RPC
/// method.
pub trait Bundle<P: Platform>:
	Serialize + DeserializeOwned + Clone + Debug + Send + Sync + 'static
{
	type PostExecutionError: core::error::Error + Send + Sync + 'static;

	/// Access to the transactions that are part of this bundle.
	fn transactions(&self) -> &[Recovered<types::Transaction<P>>];

	/// Returns a new bundle with the exact configuration and metadata as this one
	/// but without the transaction with the given hash. The returned bundle must
	/// maintain the same order of transactions as the original bundle.
	///
	/// The system guarantees that this function will not be called for a
	/// transaction that is not in the bundle, or a transaction that is not
	/// optional or the last remaining transaction in the bundle.
	#[must_use]
	fn without_transaction(self, tx: TxHash) -> Self;

	/// Checks if the bundle is eligible for inclusion in the block
	/// before executing any of its transactions.
	fn is_eligible(&self, block: &BlockContext<P>) -> Eligibility;

	/// Optional check that allows the bundle to check if it knows for sure that
	/// it will never be eligible for inclusion in any future block that is a
	/// descendant of the given header.
	///
	/// Implementing this method is optional for bundles but it gives the ability
	/// to filter out bundles at the RPC level before they are sent to the pool.
	fn is_permanently_ineligible(
		&self,
		_: &SealedHeader<types::Header<P>>,
	) -> bool {
		false
	}

	/// Checks if a transaction with a given hash is allowed to not have a
	/// successful execution result for this bundle.
	///
	/// the `tx` is the hash of the transaction to check and it is guaranteed to
	/// be hash of one of the transactions returned by `transactions()`.
	fn is_allowed_to_fail(&self, tx: TxHash) -> bool;

	/// Checks if a transaction with the given hash may be removed from the
	/// bundle without affecting its validity.
	fn is_optional(&self, tx: TxHash) -> bool;

	/// An optional check for bundle implementations that have validity
	/// requirements on the resulting state. For example there may be an
	/// implementation that requires a certain minimal balance in some account
	/// after the bundle is executed. The state that is passed in this method
	/// contains only entries that were modified or created by executing
	/// transactions from this bundle.
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
}

/// The eligibility of a bundle for inclusion in a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
	/// The bundle is eligible for inclusion in this given block. This does not
	/// mean that the bundle will actually be included, but only that static
	/// checks that do not require execution checks have passed.
	///
	/// Bundles in this state if not included in the block should be kept in the
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
	/// to be included in any future blocks.
	///
	/// Once a bundle returns this state, it should never return any other
	/// eligibility state.
	PermanentlyIneligible,
}

impl Eligibility {
	/// Returns `Some(f())` if the eligibility is `Eligible`, otherwise returns
	/// `None`.
	pub fn then<T, F: FnOnce() -> T>(self, f: F) -> Option<T> {
		if self == Eligibility::Eligible {
			Some(f())
		} else {
			None
		}
	}

	/// Returns `Some(t)` if the eligibility is `Eligible`, otherwise returns
	/// `None`.
	pub fn then_some<T>(self, t: T) -> Option<T> {
		if self == Eligibility::Eligible {
			Some(t)
		} else {
			None
		}
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

/// A bundle of transactions that adheres to the [Flashbots specification].
/// see: <https://docs.flashbots.net/flashbots-auction/searchers/advanced/rpc-endpoint#eth_sendbundle>
///
/// By default this is the bundle type that is used by both the `Ethereum` and
/// `Optimism` platforms.
#[derive(Debug, Clone)]
pub struct FlashbotsBundle<P: Platform> {
	inner: EthSendBundle,
	recovered: Vec<Recovered<types::Transaction<P>>>,
}

impl<P: Platform> Default for FlashbotsBundle<P> {
	fn default() -> Self {
		Self {
			inner: EthSendBundle::default(),
			recovered: Vec::new(),
		}
	}
}

impl<P: Platform> FlashbotsBundle<P> {
	pub const fn inner(&self) -> &EthSendBundle {
		&self.inner
	}

	pub fn into_inner(self) -> EthSendBundle {
		self.inner
	}

	#[must_use]
	pub fn with_transaction(
		mut self,
		tx: Recovered<types::Transaction<P>>,
	) -> Self {
		if !self.recovered.iter().any(|t| t.tx_hash() == tx.tx_hash()) {
			let txbytes = tx.encoded_2718();
			self.inner.txs.push(txbytes.into());
			self.recovered.push(tx);
		}
		self
	}

	#[must_use]
	pub fn with_optional_transaction(
		self,
		tx: Recovered<types::Transaction<P>>,
	) -> Self {
		let hash = *tx.tx_hash();
		self.with_transaction(tx).mark_as_optional(hash)
	}

	#[must_use]
	pub fn mark_as_revertable(mut self, tx: TxHash) -> Self {
		if !self.inner.reverting_tx_hashes.contains(&tx) {
			self.inner.reverting_tx_hashes.push(tx);
		}
		self
	}

	#[must_use]
	pub fn mark_as_optional(mut self, tx: TxHash) -> Self {
		if !self.inner.dropping_tx_hashes.contains(&tx) {
			self.inner.dropping_tx_hashes.push(tx);
		}
		self
	}

	#[must_use]
	pub fn with_max_timestamp(mut self, max_ts: u64) -> Self {
		self.inner.max_timestamp = Some(max_ts);
		self
	}

	#[must_use]
	pub fn with_min_timestamp(mut self, min_ts: u64) -> Self {
		self.inner.min_timestamp = Some(min_ts);
		self
	}

	#[must_use]
	pub fn with_block_number(mut self, block_number: u64) -> Self {
		self.inner.block_number = block_number;
		self
	}
}

impl<P: Platform> Bundle<P> for FlashbotsBundle<P> {
	type PostExecutionError = Infallible;

	fn transactions(&self) -> &[Recovered<types::Transaction<P>>] {
		&self.recovered
	}

	fn without_transaction(self, tx: TxHash) -> Self {
		let mut bundle = self;
		if let Some(position) =
			bundle.recovered.iter().position(|t| *t.tx_hash() == tx)
		{
			bundle.inner.txs.remove(position);
			bundle.recovered.remove(position);
		}

		bundle
			.reverting_tx_hashes
			.retain(|&reverting_tx| reverting_tx != tx);

		bundle
			.dropping_tx_hashes
			.retain(|&dropping_tx| dropping_tx != tx);

		bundle
	}

	fn is_eligible(&self, block: &BlockContext<P>) -> Eligibility {
		if self.transactions().is_empty() {
			// empty bundles are never eligible
			return Eligibility::PermanentlyIneligible;
		}

		if self
			.max_timestamp
			.is_some_and(|max_ts| max_ts > block.timestamp())
		{
			// this bundle will never be eligible for inclusion anymore
			return Eligibility::PermanentlyIneligible;
		}

		if self
			.min_timestamp
			.is_some_and(|min_ts| min_ts < block.timestamp())
		{
			// this bundle is not eligible yet
			return Eligibility::TemporarilyIneligible;
		}

		if self.block_number != 0 && self.block_number > block.parent().number() {
			// this bundle is not eligible yet
			return Eligibility::TemporarilyIneligible;
		}

		if self.block_number != 0 && self.block_number < block.parent().number() {
			// this bundle will never be eligible for inclusion anymore
			return Eligibility::PermanentlyIneligible;
		}

		Eligibility::Eligible
	}

	fn is_permanently_ineligible(
		&self,
		block: &SealedHeader<types::Header<P>>,
	) -> bool {
		if self.transactions().is_empty() {
			// empty bundles are never eligible
			return true;
		}

		if self
			.max_timestamp
			.is_some_and(|max_ts| max_ts < block.timestamp())
		{
			return true;
		}

		if self.block_number != 0 && self.block_number < block.number() {
			return true;
		}

		false
	}

	fn is_allowed_to_fail(&self, tx: TxHash) -> bool {
		self.reverting_tx_hashes.contains(&tx)
	}

	fn is_optional(&self, tx: TxHash) -> bool {
		self.dropping_tx_hashes.contains(&tx)
	}

	fn hash(&self) -> B256 {
		let mut hasher = Keccak256::default();
		for tx in self.transactions() {
			hasher.update(tx.tx_hash());
		}
		hasher.finalize()
	}
}

#[derive(Debug, thiserror::Error)]
pub enum BundleConversionError {
	#[error("EIP-2718 decoding error: {0}")]
	Decoding(#[from] Eip2718Error),

	#[error("Invalid transaction signature: {0}")]
	Signature(#[from] RecoveryError),
}

impl<P: Platform> Serialize for FlashbotsBundle<P> {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		self.inner.serialize(serializer)
	}
}

impl<'de, P: Platform> Deserialize<'de> for FlashbotsBundle<P> {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let inner = EthSendBundle::deserialize(deserializer)?;
		Self::try_from(inner).map_err(serde::de::Error::custom)
	}
}

impl<P: Platform> Deref for FlashbotsBundle<P> {
	type Target = EthSendBundle;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<P: Platform> DerefMut for FlashbotsBundle<P> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

impl<P: Platform> TryFrom<EthSendBundle> for FlashbotsBundle<P> {
	type Error = BundleConversionError;

	fn try_from(inner: EthSendBundle) -> Result<Self, Self::Error> {
		let txs = inner
			.txs
			.iter()
			.map(|tx| types::Transaction::<P>::decode_2718(&mut &tx[..]))
			.collect::<Result<Vec<_>, _>>()?;

		let recovered = txs
			.into_iter()
			.map(SignerRecoverable::try_into_recovered)
			.collect::<Result<Vec<_>, _>>()?;

		Ok(Self { inner, recovered })
	}
}
