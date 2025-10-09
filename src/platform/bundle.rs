use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::{BlockHeader, transaction::TxHashRef},
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
/// Examples of bundles are objects sent through the [`eth_sendBundle`] JSON-RPC
/// endpoint.
///
/// [`eth_sendBundle`]: https://docs.flashbots.net/flashbots-auction/searchers/advanced/rpc-endpoint#eth_sendbundle
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
	fn is_allowed_to_fail(&self, tx: TxHash) -> bool;

	/// Checks if a transaction with the given hash may be removed from the
	/// bundle without affecting its validity.
	fn is_optional(&self, tx: TxHash) -> bool;

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

/// A [Bundle] that follows Flashbots [`eth_sendBundle`] specification.
///
/// Default bundle type used by both `Ethereum` and `Optimism` platforms.
///
/// [`eth_sendBundle`]: https://docs.flashbots.net/flashbots-auction/searchers/advanced/rpc-endpoint#eth_sendbundle
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
			let tx_bytes = tx.encoded_2718();
			self.inner.txs.push(tx_bytes.into());
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
		self.eligibility_at(block.timestamp(), block.number())
	}

	fn is_permanently_ineligible(
		&self,
		block: &SealedHeader<types::Header<P>>,
	) -> bool {
		matches!(
			self.eligibility_at(block.timestamp(), block.number()),
			Eligibility::PermanentlyIneligible
		)
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
		hasher.update(self.block_number.to_be_bytes());
		if let Some(min_ts) = self.min_timestamp {
			hasher.update(min_ts.to_be_bytes());
		}
		if let Some(max_ts) = self.max_timestamp {
			hasher.update(max_ts.to_be_bytes());
		}
		for tx in &self.reverting_tx_hashes {
			hasher.update(tx);
		}
		if let Some(replacement_uuid) = &self.replacement_uuid {
			hasher.update(replacement_uuid);
		}
		for tx in &self.dropping_tx_hashes {
			hasher.update(tx);
		}
		if let Some(refund_percent) = self.refund_percent {
			hasher.update(refund_percent.to_be_bytes());
		}
		if let Some(refund_recipient) = &self.refund_recipient {
			hasher.update(refund_recipient);
		}
		for tx in &self.refund_tx_hashes {
			hasher.update(tx);
		}
		// extra fields not taken into account in the hash
		hasher.finalize()
	}
}

impl<P: Platform> FlashbotsBundle<P> {
	fn eligibility_at(&self, timestamp: u64, number: u64) -> Eligibility {
		// Permanent ineligibility checked first
		if self.transactions().is_empty() {
			// empty bundles are never eligible
			return Eligibility::PermanentlyIneligible;
		}

		if self.max_timestamp.is_some_and(|max_ts| max_ts < timestamp) {
			return Eligibility::PermanentlyIneligible;
		}

		if self.block_number != 0 && self.block_number < number {
			return Eligibility::PermanentlyIneligible;
		}

		// Temporary ineligibility checked next
		if self.min_timestamp.is_some_and(|min_ts| min_ts > timestamp) {
			return Eligibility::TemporarilyIneligible;
		}

		if self.block_number != 0 && self.block_number > number {
			return Eligibility::TemporarilyIneligible;
		}

		// assertions:
		// - transaction count > 0
		// - min_timestamp < timestamp < max_timestamp
		// - block_number == number
		Eligibility::Eligible
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
