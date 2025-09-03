use {
	super::*,
	crate::test_utils::*,
	alloy::{
		network::{TransactionBuilder, TransactionResponse},
		optimism::consensus::DEPOSIT_TX_TYPE_ID,
		primitives::U256,
	},
	reth::rpc::api::EthBundleApiClient,
	tracing::info,
};

/// This test ensures that transactions make their way to a block when we are
/// using the order pool. This sends only one tx with nonce 0, so we are not
/// testing nonce dependencies or ordering here.
#[rblib_test(Ethereum, Optimism)]
async fn one_tx_smoke<P: TestablePlatform>() {
	let pool = OrderPool::<P>::default();
	let pipeline = Pipeline::default().with_step(AppendOrders::from_pool(&pool));
	let node = P::create_test_node_with_pool(pipeline, pool).await.unwrap();

	let tx = node.build_tx().transfer().with_value(U256::from(1001));
	let tx = node.send_tx(tx).await.unwrap();

	let block = node.next_block().await.unwrap();
	info!("block built: {block:#?}");

	assert!(
		block.includes(tx.tx_hash()),
		"Block should include the transaction"
	);

	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), 1);
	});

	if_platform!(Optimism => {
		assert_eq!(block.tx_count(), 2, "Optimism block should have the sequencer transaction");

		assert_eq!(
			block.tx(0).unwrap().transaction_type().unwrap(),
			DEPOSIT_TX_TYPE_ID,
			"Optimism sequencer transaction should be a deposit tx"
		);

		assert_eq!(
			block.tx(1).unwrap().tx_hash(),
			*tx.tx_hash(),
			"The second transaction in the block should be the one we sent"
		);
	});
}

#[rblib_test(Ethereum, Optimism)]
async fn one_bundle_smoke<P: TestablePlatform>() {
	let pool = OrderPool::<P>::default();
	let pipeline = Pipeline::default().with_step(AppendOrders::from_pool(&pool));
	let node = P::create_test_node_with_pool(pipeline, pool).await.unwrap();

	let tx1: types::Transaction<P> = node
		.build_tx()
		.transfer()
		.with_from(FundedAccounts::address(0))
		.with_value(U256::from(1001))
		.with_gas_price(1_000_000_000)
		.with_gas_limit(21_000)
		.with_max_priority_fee_per_gas(1_000_000)
		.with_max_fee_per_gas(2_000_000)
		.with_nonce(0)
		.build_with_known_signer()
		.unwrap()
		.into();

	let tx2: types::Transaction<P> = node
		.build_tx()
		.transfer()
		.with_from(FundedAccounts::address(1))
		.with_value(U256::from(1002))
		.with_gas_price(1_000_000_000)
		.with_gas_limit(21_000)
		.with_max_priority_fee_per_gas(1_000_000)
		.with_max_fee_per_gas(2_000_000)
		.with_nonce(0)
		.build_with_known_signer()
		.unwrap()
		.into();

	let bundle = FlashbotsBundle::<P>::default()
		.with_transaction(tx1.try_clone_into_recovered().unwrap())
		.with_transaction(tx2.try_clone_into_recovered().unwrap());

	let rpc_client = node.rpc_client().await.unwrap();
	let res = EthBundleApiClient::send_bundle(&rpc_client, bundle.into()).await;
	tracing::info!(">---> sendBudle response: {res:#?}");

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

		assert_eq!(
			block.tx(0).unwrap().transaction_type().unwrap(),
			DEPOSIT_TX_TYPE_ID,
			"Optimism sequencer transaction should be a deposit tx"
		);

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
