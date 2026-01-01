//! Minimal example of the Progressive Payload API for Ethereum.
//!
//! In this example are creating a new [`BlockContext`] instance using the
//! test-utils provided helpers for creating mock instances.

use rblib::{
	alloy::{
		consensus::{Transaction, transaction::Recovered},
		primitives::U256,
		signers::local::PrivateKeySigner,
	},
	prelude::*,
	test_utils::{
		BlockContextMocked,
		FundedAccounts,
		transfer_tx as test_transfer_tx,
	},
};

fn main() -> eyre::Result<()> {
	// This is the entry point of the payload building API. We construct a
	// building context for a given block and attributes.
	let block = BlockContext::<Ethereum>::mocked();

	// Next we progressively build the payload by creating checkpoints that have
	// state mutations applied to them.
	let start = block.start();

	// create transactions that transfer some eth to random accounts.
	let tx1 = transfer_tx(&FundedAccounts::signer(0), 0, U256::from(50_000u64));
	let tx2 = transfer_tx(&FundedAccounts::signer(1), 0, U256::from(25_000u64));
	let tx3 = transfer_tx(&FundedAccounts::signer(0), 1, U256::from(10_000u64));
	let tx4 = transfer_tx(&FundedAccounts::signer(1), 1, U256::from(5_000u64));

	// A bundle is an atomic set of transactions that must be included together
	// consecutively in the same order as specified.
	let bundle = FlashbotsBundle::default()
		.with_transaction(tx2)
		.with_transaction(tx3);

	// Checkpoints can be applied on top of each other, creating a progressive
	// history of state changes.
	let payload = start.apply(tx1)?.apply(bundle)?.apply(tx4)?;
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

	assert_eq!(transactions.len(), 4);
	assert_eq!(transactions[0].value(), U256::from(50_000u64));
	assert_eq!(transactions[1].value(), U256::from(25_000u64));
	assert_eq!(transactions[2].value(), U256::from(10_000u64));
	assert_eq!(transactions[3].value(), U256::from(5_000u64));

	Ok(())
}

fn transfer_tx(
	signer: &PrivateKeySigner,
	nonce: u64,
	value: U256,
) -> Recovered<types::Transaction<Ethereum>> {
	test_transfer_tx::<Ethereum>(signer, nonce, value)
}
