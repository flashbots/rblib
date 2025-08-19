use crate::{
	alloy::{
		network::{TransactionBuilder, TxSignerSync},
		optimism::{consensus::OpTxEnvelope, rpc_types::OpTransactionRequest},
		primitives::{Address, U256},
		signers::local::PrivateKeySigner,
	},
	reth::{
		ethereum::primitives::SignedTransaction,
		optimism::primitives::OpTransactionSigned, primitives::Recovered,
	},
};

pub trait PrivateKeySignerOpExt {
	fn test_tx_with_nonce(&self, nonce: u64) -> Recovered<OpTxEnvelope>;
}

impl PrivateKeySignerOpExt for PrivateKeySigner {
	fn test_tx_with_nonce(&self, nonce: u64) -> Recovered<OpTxEnvelope> {
		let mut tx = OpTransactionRequest::default()
			.with_nonce(nonce)
			.with_to(Address::random())
			.value(U256::from(44100u64))
			.with_gas_price(1_000_000_000)
			.with_gas_limit(21_000)
			.with_max_priority_fee_per_gas(1_000_000)
			.with_max_fee_per_gas(2_000_000)
			.build_unsigned()
			.expect("valid transaction request");

		let sig = self
			.sign_transaction_sync(&mut tx)
			.expect("signing should succeed");

		OpTransactionSigned::new_unhashed(tx, sig).with_signer(self.address())
	}
}
