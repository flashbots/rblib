use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::{BlockHeader, transaction::TxHashRef},
		eips::{Decodable2718, Encodable2718},
		primitives::{B256, Keccak256, TxHash},
	},
	core::{
		convert::Infallible,
		fmt::Debug,
		ops::{Deref, DerefMut},
	},
	reth::{
		ethereum::primitives::{Recovered, SignerRecoverable},
		primitives::SealedHeader,
		rpc::types::mev::EthSendBundle,
	},
	reth_ethereum::primitives::WithEncoded,
	serde::{Deserialize, Deserializer, Serialize, Serializer},
};

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

	fn is_allowed_to_fail(&self, tx: &TxHash) -> bool {
		self.reverting_tx_hashes.contains(tx)
	}

	fn is_optional(&self, tx: &TxHash) -> bool {
		self.dropping_tx_hashes.contains(tx)
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

	/// Returns an iterator that yields the bundle's transactions in a format
	/// ready for execution.
	fn transactions_encoded(
		&self,
	) -> Box<
		dyn Iterator<Item = WithEncoded<&Recovered<types::Transaction<P>>>> + '_,
	> {
		Box::new(
			self
				.recovered
				.iter()
				.zip(self.inner.txs.iter())
				.map(|(tx, bytes)| WithEncoded::new(bytes.clone(), tx)),
		)
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

/// `PartialEq` by bundle hash
impl<P: Platform> PartialEq for FlashbotsBundle<P> {
	fn eq(&self, other: &Self) -> bool {
		self.hash() == other.hash()
	}
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
