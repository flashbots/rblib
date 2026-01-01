use {
	crate::{
		alloy::{
			consensus::BlockHeader,
			network::{BlockResponse, TransactionBuilder},
			primitives::U256,
		},
		pool::OrderPool,
		prelude::*,
		steps::*,
		test_utils::*,
	},
	tracing::info,
};

#[rblib_test(Ethereum, Optimism)]
async fn empty_payload_when_all_txs_revert_loop<P: TestablePlatform>() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::default().with_pipeline(
		Loop,
		(
			AppendOrders::from_pool(&pool),
			OrderByPriorityFee::default(),
			RemoveRevertedTransactions::default(),
		),
	);

	let node = P::create_test_node(pipeline).await.unwrap();
	pool.attach_to_test_node(&node).unwrap();

	let mut reverts = vec![];
	for i in 0..10 {
		let tx = node.build_tx().reverting().with_value(U256::from(3000 + i));
		reverts.push(*node.send_tx(tx).await.unwrap().tx_hash());
	}

	let block = node.next_block().await.unwrap();
	assert_eq!(block.header().number(), 1);

	if_platform!(Ethereum => {
		// Ethereum should not include any transactions
		assert!(block.is_empty(), "Block should be empty");
	});

	if_platform!(Optimism => {
		// Optimism should only include a sequencer deposit transaction
		assert_eq!(block.tx_count(), 1,
			"Optimism block should have only one sequencer transaction");
	});
}

#[rblib_test(Ethereum, Optimism)]
async fn transfers_included_reverts_excluded_loop<P: TestablePlatform>() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::default().with_pipeline(
		Loop,
		(
			AppendOrders::from_pool(&pool).with_max_new_orders(1),
			OrderByPriorityFee::default(),
			RemoveRevertedTransactions::default(),
		),
	);

	let node = P::create_test_node(pipeline).await.unwrap();
	pool.attach_to_test_node(&node).unwrap();

	let mut transfers = vec![];
	for i in 0..10 {
		let tx = node.build_tx().transfer().with_value(U256::from(i));
		transfers.push(*node.send_tx(tx).await.unwrap().tx_hash());
	}

	let mut reverts = vec![];
	for i in 0..4 {
		let tx = node.build_tx().reverting().with_value(U256::from(3000 + i));
		reverts.push(*node.send_tx(tx).await.unwrap().tx_hash());
	}

	let block = node.next_block().await.unwrap();

	info!("Block built: {block:#?}");

	assert!(
		block.includes(&transfers),
		"Block should include all valid transfers"
	);

	assert!(
		!block.includes(&reverts),
		"Block should not include any reverts"
	);

	// non-reverting transactions
	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), transfers.len(),
			"Ethereum block should include all transfers and no reverts");
	});

	if_platform!(Optimism => {
		assert_eq!(block.tx_count(), transfers.len() + 1,
			"Optimism block should have only one sequencer transaction \
			 and all transfers and no reverts");
	});
}

#[rblib_test(Ethereum, Optimism)]
async fn transfers_included_reverts_excluded_flat<P: TestablePlatform>() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::default()
		.with_step(AppendOrders::from_pool(&pool))
		.with_step(OrderByPriorityFee::default())
		.with_step(RemoveRevertedTransactions::default());

	let node = P::create_test_node(pipeline).await.unwrap();
	pool.attach_to_test_node(&node).unwrap();

	let mut transfers = vec![];
	for i in 0..10 {
		let tx = node.build_tx().transfer().with_value(U256::from(i));
		transfers.push(*node.send_tx(tx).await.unwrap().tx_hash());
	}

	let mut reverts = vec![];
	for i in 0..4 {
		let tx = node.build_tx().reverting().with_value(U256::from(3000 + i));
		reverts.push(*node.send_tx(tx).await.unwrap().tx_hash());
	}

	let block = node.next_block().await.unwrap();

	info!("Block built: {block:#?}");

	assert!(
		block.includes(&transfers),
		"Block should include all valid transfers"
	);

	assert!(
		!block.includes(&reverts),
		"Block should not include any reverts"
	);

	// non-reverting transactions
	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), transfers.len(),
			"Ethereum block should include all transfers and no reverts");
	});

	if_platform!(Optimism => {
		assert_eq!(block.tx_count(), transfers.len() + 1,
			"Optimism block should have only one sequencer transaction \
			 and all transfers and no reverts");
	});
}
