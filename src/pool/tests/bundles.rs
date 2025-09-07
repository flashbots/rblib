use {
	super::*,
	alloy::{
		network::{TransactionBuilder, TransactionResponse},
		primitives::U256,
	},
	reth::{
		ethereum::primitives::SignedTransaction,
		rpc::api::EthBundleApiClient,
	},
};

#[rblib_test(Ethereum, Optimism)]
async fn one_bundle_two_txs_zero_nonces<P: TestablePlatform>() {
	let pool = OrderPool::<P>::default();
	let pipeline = Pipeline::default().with_step(AppendOrders::from_pool(&pool));
	let node = P::create_test_node_with_pool(pipeline, pool).await.unwrap();

	let tx1 = node
		.prepare_signed_tx(
			node
				.build_tx()
				.transfer()
				.with_from(FundedAccounts::address(0))
				.with_value(U256::from(1001)),
			FundedAccounts::signer(0),
		)
		.await
		.unwrap();

	let tx2 = node
		.prepare_signed_tx(
			node
				.build_tx()
				.transfer()
				.with_from(FundedAccounts::address(1))
				.with_value(U256::from(1002)),
			FundedAccounts::signer(1),
		)
		.await
		.unwrap();

	let bundle = FlashbotsBundle::<P>::default()
		.with_transaction(tx1.clone())
		.with_transaction(tx2.clone());
	let bundle_hash = bundle.hash();

	let rpc_client = node.rpc_client().await.unwrap();
	let res = EthBundleApiClient::send_bundle(&rpc_client, bundle.into())
		.await
		.unwrap();

	assert_eq!(res.bundle_hash, bundle_hash);

	let block = node.next_block().await.unwrap();
	info!("block built: {block:#?}");

	assert!(
		block.includes(tx1.tx_hash()),
		"Block should include the transaction"
	);

	assert!(
		block.includes(tx2.tx_hash()),
		"Block should include the transaction"
	);

	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), 2);
	});

	if_platform!(Optimism => {
		assert_eq!(block.tx_count(), 3, "Optimism block should have the sequencer transaction");

		assert_has_sequencer_tx!(&block);

		assert_eq!(
			block.tx(1).unwrap().tx_hash(),
			*tx1.tx_hash(),
		);

		assert_eq!(
			block.tx(2).unwrap().tx_hash(),
			*tx2.tx_hash(),
		);
	});
}
