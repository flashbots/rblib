//! Example of a custom platform extending an existing platform (`Optimism`)
//! with a custom bundle type.
//!
//! In this example, we define a new platform type that inherits all properties
//! and behaviors of the `Optimism` platform but uses a custom bundle type that
//! allows users to define the minimum coinbase profit this bundle generates for
//! it to be considered valid.

use {
	alloy::{
		consensus::Transaction,
		network::{TransactionBuilder, TxSignerSync},
		optimism::{consensus::OpTxEnvelope, rpc_types::OpTransactionRequest},
		primitives::{Address, B256, Keccak256, TxHash, U256},
		signers::local::PrivateKeySigner,
	},
	rblib::{alloy, prelude::*, reth, test_utils::*},
	reth::{
		ethereum::primitives::SignedTransaction,
		optimism::primitives::OpTransactionSigned,
		primitives::Recovered,
		providers::StateProvider,
		revm::db::BundleState,
	},
	serde::{Deserialize, Serialize},
	std::sync::{Arc, LazyLock},
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct CustomPlatform;

impl Platform for CustomPlatform {
	type Bundle = CustomBundleType;
	type DefaultLimits = types::DefaultLimits<Optimism>;
	type EvmConfig = types::EvmConfig<Optimism>;
	type ExtraLimits = types::ExtraLimits<Optimism>;
	type NodeTypes = types::NodeTypes<Optimism>;
	type PooledTransaction = types::PooledTransaction<Optimism>;

	fn evm_config<P>(
		chainspec: Arc<types::ChainSpec<Optimism>>,
	) -> Self::EvmConfig {
		Optimism::evm_config::<Self>(chainspec)
	}

	fn next_block_environment_context<P>(
		chainspec: &types::ChainSpec<Optimism>,
		parent: &types::Header<Optimism>,
		attributes: &types::PayloadBuilderAttributes<Optimism>,
	) -> types::NextBlockEnvContext<Optimism> {
		Optimism::next_block_environment_context::<Self>(
			chainspec, parent, attributes,
		)
	}

	fn build_payload<P>(
		payload: Checkpoint<P>,
		provider: &dyn StateProvider,
	) -> Result<types::BuiltPayload<Self>, PayloadBuilderError>
	where
		P: traits::PlatformExecBounds<Self>,
	{
		Optimism::build_payload::<P>(payload, provider)
	}
}

impl PlatformWithTestnet for CustomPlatform {
	fn dev_chainspec() -> Arc<types::ChainSpec<Self>> {
		LazyLock::force(&crate::reth::optimism::chainspec::OP_DEV)
			.clone()
			.with_funded_accounts()
	}
}

fn main() -> eyre::Result<()> {
	// Construct a mock build context for the custom platform.
	let block = BlockContext::<CustomPlatform>::mocked();

	// begin building the payload by creating the first checkpoint for the block.
	let start = block.start();

	// create transactions that transfer some eth to random accounts.
	let tx1 = transfer_tx(&FundedAccounts::signer(0), 0, U256::from(50_000u64));
	let tx2 = transfer_tx(&FundedAccounts::signer(1), 0, U256::from(25_000u64));
	let tx3 = transfer_tx(&FundedAccounts::signer(0), 1, U256::from(10_000u64));
	let tx4 = transfer_tx(&FundedAccounts::signer(1), 1, U256::from(5_000u64));

	let bundle = CustomBundleType::with_min_profit(U256::from(10_000u64))
		.with_transaction(tx2)
		.with_transaction(tx3);

	let payload = start.apply(tx1)?.apply(bundle)?.apply(tx4)?;

	let tx5 = transfer_tx(&FundedAccounts::signer(2), 0, U256::from(15_000u64));
	let tx6 = transfer_tx(&FundedAccounts::signer(3), 0, U256::from(7_500u64));
	let failing_bundle =
		CustomBundleType::with_min_profit(U256::from(10_000_000_000_000u64))
			.with_transaction(tx5)
			.with_transaction(tx6);

	// ensure that custom bundle logic is applied correctly and bundles that do
	// not meet the minimum profit are rejected.
	assert!(matches!(
		payload.apply(failing_bundle),
		Err(ExecutionError::InvalidBundlePostExecutionState(
			MinimumProfitNotMet { .. }
		))
	));

	// build payload
	let built_payload = payload
		.build_payload()
		.expect("payload should be built successfully");

	println!("{built_payload:#?}");

	assert_eq!(built_payload.block().header().number, 1);
	assert_eq!(
		built_payload.block().header().parent_hash,
		block.parent().hash()
	);

	let transactions = &built_payload.block().body().transactions;

	assert_eq!(transactions.len(), 5); // 4 transactions + 1 op sequencer tx
	assert!(transactions[0].is_deposit()); // sequencer deposit tx
	assert_eq!(transactions[1].value(), U256::from(50_000u64));
	assert_eq!(transactions[2].value(), U256::from(25_000u64));
	assert_eq!(transactions[3].value(), U256::from(10_000u64));
	assert_eq!(transactions[4].value(), U256::from(5_000u64));

	Ok(())
}

/// This custom bundle type allows users to define the minimum coinbase profit
/// this bundle generates for it to be considered valid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CustomBundleType {
	pub txs: Vec<Recovered<types::Transaction<Optimism>>>,
	pub reverting_txs: Vec<TxHash>,
	pub dropping_txs: Vec<TxHash>,
	pub min_coinbase_profit: U256,
}

impl CustomBundleType {
	fn with_min_profit(min_profit: U256) -> Self {
		Self {
			txs: Vec::new(),
			reverting_txs: Vec::new(),
			dropping_txs: Vec::new(),
			min_coinbase_profit: min_profit,
		}
	}

	fn with_transaction(
		mut self,
		tx: Recovered<types::Transaction<Optimism>>,
	) -> Self {
		if !self.txs.iter().any(|t| t.tx_hash() == tx.tx_hash()) {
			self.txs.push(tx);
		}
		self
	}
}

impl Bundle<CustomPlatform> for CustomBundleType {
	type PostExecutionError = MinimumProfitNotMet;

	fn transactions(&self) -> &[Recovered<types::Transaction<Optimism>>] {
		&self.txs
	}

	fn without_transaction(self, tx: TxHash) -> Self {
		Self {
			txs: self.txs.into_iter().filter(|t| *t.hash() != tx).collect(),
			reverting_txs: self
				.reverting_txs
				.into_iter()
				.filter(|t| *t != tx)
				.collect(),
			dropping_txs: self
				.dropping_txs
				.into_iter()
				.filter(|t| *t != tx)
				.collect(),
			min_coinbase_profit: self.min_coinbase_profit,
		}
	}

	fn is_eligible(&self, _: &BlockContext<CustomPlatform>) -> Eligibility {
		Eligibility::Eligible
	}

	fn is_allowed_to_fail(&self, tx: &TxHash) -> bool {
		self.reverting_txs.contains(tx)
	}

	fn is_optional(&self, tx: &TxHash) -> bool {
		self.dropping_txs.contains(tx)
	}

	fn validate_post_execution(
		&self,
		state: &BundleState,
		block: &BlockContext<CustomPlatform>,
	) -> Result<(), Self::PostExecutionError> {
		let coinbase = block
			.attributes()
			.payload_attributes
			.suggested_fee_recipient;

		let Some(coinbase) = state.account(&coinbase) else {
			if self.min_coinbase_profit > U256::ZERO {
				return Err(MinimumProfitNotMet {
					min: self.min_coinbase_profit,
					actual: U256::ZERO,
				});
			}
			return Ok(());
		};

		let current_balance = coinbase
			.info
			.as_ref()
			.map(|info| info.balance)
			.unwrap_or_default();

		let previous_balance = coinbase
			.original_info
			.as_ref()
			.map(|info| info.balance)
			.unwrap_or_default();

		let profit = current_balance.saturating_sub(previous_balance);

		if profit < self.min_coinbase_profit {
			return Err(MinimumProfitNotMet {
				min: self.min_coinbase_profit,
				actual: profit,
			});
		}

		Ok(())
	}

	fn hash(&self) -> B256 {
		let mut hasher = Keccak256::default();

		for tx in &self.txs {
			hasher.update(tx.tx_hash());
		}

		for tx in &self.reverting_txs {
			hasher.update(tx);
		}

		for tx in &self.dropping_txs {
			hasher.update(tx);
		}

		if self.min_coinbase_profit > U256::ZERO {
			hasher.update(self.min_coinbase_profit.as_le_slice());
		}

		hasher.finalize()
	}
}

fn transfer_tx(
	signer: &PrivateKeySigner,
	nonce: u64,
	value: U256,
) -> Recovered<OpTxEnvelope> {
	let mut tx = OpTransactionRequest::default()
		.with_nonce(nonce)
		.with_to(Address::random())
		.value(value)
		.with_gas_price(1_000_000_000)
		.with_gas_limit(21_000)
		.with_max_priority_fee_per_gas(1_000_000)
		.with_max_fee_per_gas(2_000_000)
		.build_unsigned()
		.expect("valid transaction request");

	let sig = signer
		.sign_transaction_sync(&mut tx)
		.expect("signing should succeed");

	OpTransactionSigned::new_unhashed(tx, sig) //
		.with_signer(signer.address())
}

impl PartialEq for CustomBundleType {
	fn eq(&self, other: &Self) -> bool {
		self.hash() == other.hash()
	}
}

#[derive(Debug)]
struct MinimumProfitNotMet {
	min: U256,
	actual: U256,
}

impl core::fmt::Display for MinimumProfitNotMet {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		write!(
			f,
			"Minimum profit not met: expected >= {}, got {}",
			self.min, self.actual
		)
	}
}
impl core::error::Error for MinimumProfitNotMet {}
