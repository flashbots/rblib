use {
	super::*,
	alloy::{
		consensus::SignableTransaction,
		network::{TransactionBuilder, TxSignerSync},
		primitives::{Address, U256},
		signers::local::PrivateKeySigner,
	},
	reth::{ethereum::primitives::SignedTransaction, primitives::Recovered},
};

pub fn make_tx<P: PlatformWithRpcTypes>(
	signer: &PrivateKeySigner,
	nonce: u64,
) -> Recovered<types::Transaction<P>> {
	let mut tx = types::TransactionRequest::<P>::default()
		.with_nonce(nonce)
		.with_to(Address::random())
		.with_value(U256::from(1_000u64))
		.with_gas_price(1_000_000_000)
		.with_gas_limit(21_000)
		.with_max_priority_fee_per_gas(1_000_000)
		.with_max_fee_per_gas(2_000_000)
		.build_unsigned()
		.expect("valid transaction request");

	let sig = signer
		.sign_transaction_sync(&mut tx)
		.expect("signing should succeed");

	let envelope: types::TxEnvelope<P> = tx.into_signed(sig).into();
	let tx: types::Transaction<P> = envelope.into();
	tx.try_into_recovered().unwrap()
}

pub struct SignerIdAndNonce(pub u32, pub u64);

pub fn make_order<P: PlatformWithRpcTypes<Bundle = FlashbotsBundle<P>>>(
	signers_and_nonces: &[SignerIdAndNonce],
) -> Order<P> {
	let transactions = signers_and_nonces
		.iter()
		.map(|SignerIdAndNonce(id, nonce)| {
			make_tx::<P>(&FundedAccounts::signer(*id), *nonce)
		})
		.collect::<Vec<_>>();

	if transactions.len() == 1 {
		Order::Transaction(transactions.into_iter().next().unwrap())
	} else {
		Order::Bundle(
			transactions
				.into_iter()
				.fold(FlashbotsBundle::default(), |b, tx| b.with_transaction(tx)),
		)
	}
}
