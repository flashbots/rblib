use {
	super::*,
	crate::test_utils::*,
	alloy::{
		network::{TransactionBuilder, TransactionResponse},
		optimism::consensus::DEPOSIT_TX_TYPE_ID,
		primitives::U256,
	},
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

	let tx = node.build_tx().transfer().with_value(U256::from(1000));
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
