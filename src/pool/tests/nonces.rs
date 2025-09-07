use {
	super::*,
	alloy::{
		network::{TransactionBuilder, TransactionResponse},
		primitives::U256,
	},
};

/// This test ensures that transactions make their way to a block when we are
/// using the order pool. This sends only one tx with nonce 0, so we are not
/// testing nonce dependencies or ordering here.
#[rblib_test(Ethereum, Optimism)]
async fn one_tx_zero_nonce<P: TestablePlatform>() {
	let pool = OrderPool::<P>::default();
	let pipeline = Pipeline::default().with_step(AppendOrders::from_pool(&pool));
	let node = P::create_test_node_with_pool(pipeline, pool).await.unwrap();

	let tx = node.build_tx().transfer().with_value(U256::from(1001));
	let tx = node.send_tx(tx).await.unwrap();

	let block = node.next_block().await.unwrap();
	info!("block built: {block:#?}");

	assert!(block.includes(tx.tx_hash()),);

	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), 1);
	});

	if_platform!(Optimism => {
		assert_eq!(block.tx_count(), 2, "Optimism block should have the sequencer transaction");

		assert_has_sequencer_tx!(&block);
		assert_eq!(block.tx(1).unwrap().tx_hash(), *tx.tx_hash());
	});
}

#[rblib_test(Ethereum, Optimism)]
async fn two_txs_consecutive_nonces<P: TestablePlatform>() {
	let pool = OrderPool::<P>::default();
	let pipeline = Pipeline::default().with_step(AppendOrders::from_pool(&pool));
	let node = P::create_test_node_with_pool(pipeline, pool).await.unwrap();

	let tx1 = node
		.build_tx()
		.transfer()
		.with_value(U256::from(1001))
		.with_nonce(0)
		.with_from(FundedAccounts::address(0));
	let tx1 = node.send_tx(tx1).await.unwrap();

	let tx2 = node
		.build_tx()
		.transfer()
		.with_value(U256::from(1001))
		.with_nonce(1)
		.with_from(FundedAccounts::address(0));
	let tx2 = node.send_tx(tx2).await.unwrap();

	let block = node.next_block().await.unwrap();
	info!("block built: {block:#?}");

	assert!(block.includes(tx1.tx_hash()));
	assert!(block.includes(tx2.tx_hash()));

	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), 2);
	});

	if_platform!(Optimism => {
		assert_eq!(block.tx_count(), 3, "Optimism block should have the sequencer transaction");

		assert_has_sequencer_tx!(&block);
		assert_eq!(block.tx(1).unwrap().tx_hash(), *tx1.tx_hash());
		assert_eq!(block.tx(2).unwrap().tx_hash(), *tx2.tx_hash());
	});
}
