use {
	crate::{alloy, reth},
	alloy::{
		consensus::{EthereumTxEnvelope, TxEip4844},
		network::{TransactionBuilder, TxSignerSync},
		primitives::{Address, U256},
		signers::local::PrivateKeySigner,
	},
	reth::{
		ethereum::{TransactionSigned, primitives::SignedTransaction},
		primitives::Recovered,
		rpc::types::TransactionRequest,
	},
};

#[allow(clippy::missing_panics_doc)]
pub fn transfer_tx(
	signer: &PrivateKeySigner,
	nonce: u64,
	value: U256,
) -> Recovered<EthereumTxEnvelope<TxEip4844>> {
	let mut tx = TransactionRequest::default()
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

	TransactionSigned::new_unhashed(tx.into(), sig) //
		.with_signer(signer.address())
}
