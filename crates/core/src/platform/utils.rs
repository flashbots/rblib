//! Utilities that improve the developer experience when
//! working platform-agnostic code.

use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		network::{TransactionBuilder, TxSignerSync, UnbuiltTransactionError},
		signers::Signature,
	},
	alloy_origin::consensus::Signed,
	reth::primitives::Recovered,
};

#[derive(Debug, thiserror::Error)]
pub enum TransactionBuilderError<P: PlatformWithRpcTypes> {
	#[error("Failed to build transaction: {0:?}")]
	Build(#[from] UnbuiltTransactionError<types::RpcTypes<P>>),

	#[error("Failed to sign transaction: {0:?}")]
	Sign(#[from] alloy::signers::Error),
}

pub fn build_signed<P: PlatformWithRpcTypes>(
	tx_params: types::TransactionRequest<P>,
	signer: impl TxSignerSync<Signature>,
) -> Result<Recovered<types::Transaction<P>>, TransactionBuilderError<P>> {
	let mut unsigned_tx = tx_params.build_unsigned()?;
	let signature = signer.sign_transaction_sync(&mut unsigned_tx)?;
	let signed_tx: types::TxEnvelope<P> =
		Signed::new_unhashed(unsigned_tx, signature).into();
	let native_tx: types::Transaction<P> = signed_tx.into();
	let signer_address = signer.address();
	Ok(Recovered::new_unchecked(native_tx, signer_address))
}
