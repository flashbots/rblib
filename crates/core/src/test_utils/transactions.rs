use {
	crate::{alloy, prelude, reth, test_utils},
	alloy::{
		consensus::SignableTransaction,
		network::{TransactionBuilder, TxSignerSync},
		primitives::{Address, U256},
		signers::local::PrivateKeySigner,
	},
	prelude::*,
	reth::{ethereum::primitives::SignedTransaction, primitives::Recovered},
	test_utils::FundedAccounts,
};

/// A tx from `FundedAccounts` with `key` and `nonce`
/// Will create a transfer tx with value 1
pub fn test_tx<P: PlatformWithRpcTypes>(
	key: u32,
	nonce: u64,
) -> Recovered<types::Transaction<P>> {
	transfer_tx::<P>(&FundedAccounts::signer(key), nonce, U256::from(1u64))
}

/// `count` txs from `FundedAccounts` with `key` and `initial_nonce`
/// Will consume `count` nonce for `key`
pub fn test_txs<P: PlatformWithRpcTypes>(
	key: u32,
	initial_nonce: u64,
	count: u64,
) -> Vec<Recovered<types::Transaction<P>>> {
	(initial_nonce..initial_nonce + count)
		.map(|nonce| test_tx::<P>(key, nonce))
		.collect()
}

/// A mock `FlashbotsBundle` from `FundedAccounts` with `key` and `nonce`
/// Will insert 3 txs using [`test_tx`] and consume 3 nonces for `key`
pub fn test_bundle<P: PlatformWithRpcTypes<Bundle = FlashbotsBundle<P>>>(
	key: u32,
	nonce: u64,
) -> (FlashbotsBundle<P>, Vec<Recovered<types::Transaction<P>>>) {
	let txs = test_txs::<P>(key, nonce, 3);
	(
		FlashbotsBundle::<P>::default()
			.with_transaction(txs[0].clone())
			.with_transaction(txs[1].clone())
			.with_transaction(txs[2].clone()),
		txs,
	)
}

#[allow(clippy::missing_panics_doc)]
pub fn transfer_tx<P: PlatformWithRpcTypes>(
	signer: &PrivateKeySigner,
	nonce: u64,
	value: U256,
) -> Recovered<types::Transaction<P>> {
	let mut tx = types::TransactionRequest::<P>::default()
		.with_nonce(nonce)
		.with_to(Address::random())
		.with_value(value)
		.with_gas_price(1_000_000_000)
		.with_gas_limit(21_000)
		.with_max_priority_fee_per_gas(1_000_000)
		.with_max_fee_per_gas(2_000_000)
		.build_unsigned()
		.expect("valid transaction request");

	let sig = signer
		.sign_transaction_sync(&mut tx)
		.expect("signing should succeed");

	let signed_tx: types::TxEnvelope<P> = tx.into_signed(sig).into();
	let signed_tx: types::Transaction<P> = signed_tx.into();
	signed_tx.with_signer(signer.address())
}

/// Create a transaction that will revert when executed
#[allow(clippy::missing_panics_doc)]
pub fn reverting_tx<P: PlatformWithRpcTypes>(
	signer: &PrivateKeySigner,
	nonce: u64,
) -> Recovered<types::Transaction<P>> {
	// Create a tx that sends to an address with no code but with
	// non-zero gas limit and data, which will cause a revert
	let mut tx = types::TransactionRequest::<P>::default()
		.with_nonce(nonce)
		.with_to(Address::random())
		.with_value(U256::ZERO)
		.with_gas_price(1_000_000_000)
		.with_gas_limit(100_000)
		.with_max_priority_fee_per_gas(1_000_000)
		.with_max_fee_per_gas(2_000_000)
		.with_input(vec![0xff; 32]) // invalid data to trigger revert
		.build_unsigned()
		.expect("valid transaction request");

	let sig = signer
		.sign_transaction_sync(&mut tx)
		.expect("signing should succeed");

	let signed_tx: types::TxEnvelope<P> = tx.into_signed(sig).into();
	let signed_tx: types::Transaction<P> = signed_tx.into();
	signed_tx.with_signer(signer.address())
}

/// Create a transaction with insufficient gas that will cause an invalid tx
/// error
#[allow(clippy::missing_panics_doc)]
pub fn invalid_tx<P: PlatformWithRpcTypes>(
	signer: &PrivateKeySigner,
	nonce: u64,
) -> Recovered<types::Transaction<P>> {
	// Create a tx with insufficient gas that will fail
	let mut tx = types::TransactionRequest::<P>::default()
		.with_nonce(nonce)
		.with_to(Address::random())
		.with_value(U256::from(1u64))
		.with_gas_price(1_000_000_000)
		.with_gas_limit(1000) // Too low gas
		.with_max_priority_fee_per_gas(1_000_000)
		.with_max_fee_per_gas(2_000_000)
		.build_unsigned()
		.expect("valid transaction request");

	let sig = signer
		.sign_transaction_sync(&mut tx)
		.expect("signing should succeed");

	let signed_tx: types::TxEnvelope<P> = tx.into_signed(sig).into();
	let signed_tx: types::Transaction<P> = signed_tx.into();
	signed_tx.with_signer(signer.address())
}
