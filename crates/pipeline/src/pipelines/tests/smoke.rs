use {
	crate::{
		alloy::{
			consensus::BlockHeader,
			network::{BlockResponse, TransactionBuilder, TransactionResponse},
			optimism::consensus::DEPOSIT_TX_TYPE_ID,
			primitives::U256,
		},
		pool::OrderPool,
		prelude::*,
		reth::{
			cli::Cli,
			ethereum::node::{EthereumNode, node::EthereumAddOns},
		},
		steps::*,
		test_utils::*,
	},
	tracing::info,
};

#[rblib_test(Ethereum, Optimism)]
async fn empty_pipeline_builds_empty_payload<P: TestablePlatform>() {
	let empty_pipeline = Pipeline::default();
	let node = P::create_test_node(empty_pipeline).await.unwrap();

	for _ in 0..10 {
		let _ = node.send_tx(node.build_tx().transfer()).await.unwrap();
	}

	let block = node.next_block().await.unwrap();

	// ensure we've built the first block past genesis
	assert_eq!(block.header().number(), 1);

	if_platform!(Ethereum => {
		// Ethereum should not include any transactions
		assert!(block.is_empty(), "Block should be empty");
	});

	if_platform!(Optimism => {
		// Optimism should only include a sequencer deposit transaction
		assert_eq!(block.tx_count(), 1,
			"Optimism block should have only one sequencer transaction");

		assert_eq!(
			block.tx(0).unwrap().transaction_type().unwrap(),
			DEPOSIT_TX_TYPE_ID,
			"Optimism sequencer transaction should be a deposit tx"
		);
	});
}

#[rblib_test(Ethereum, Optimism)]
async fn pipeline_with_no_txs_builds_empty_payload<P: TestablePlatform>() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::default()
		.with_step(AppendOrders::from_pool(&pool))
		.with_step(OrderByPriorityFee::default())
		.with_step(RemoveRevertedTransactions::default());

	let node = P::create_test_node(pipeline).await.unwrap();
	pool.attach_to_test_node(&node).unwrap();

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

		assert_eq!(
			block.tx(0).unwrap().transaction_type().unwrap(),
			DEPOSIT_TX_TYPE_ID,
			"Optimism sequencer transaction should be a deposit tx"
		);
	});
}

#[rblib_test(Ethereum, Optimism)]
async fn all_transactions_included_by_one<P: TestablePlatform>() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::default().with_pipeline(
		Loop,
		(
			AppendOrders::from_pool(&pool).with_max_new_orders(1),
			OrderByPriorityFee::default(),
		),
	);

	let node = P::create_test_node(pipeline).await.unwrap();
	pool.attach_to_test_node(&node).unwrap();

	let mut transfers = vec![];
	for i in 0..10 {
		let tx = node.build_tx().transfer().with_value(U256::from(i + 1));
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
		block.includes(&reverts),
		"Block should not include any reverts"
	);

	assert_eq!(block.header().number(), 1);

	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), transfers.len() + reverts.len());
	});

	if_platform!(Optimism => {
		// Optimism should include an extra sequencer deposit transaction
		assert_eq!(block.tx_count(), transfers.len() + reverts.len() + 1);
	});
}

#[rblib_test(Ethereum, Optimism)]
async fn all_transactions_included_by_many<P: TestablePlatform>() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::default().with_pipeline(
		Loop,
		(
			AppendOrders::from_pool(&pool),
			OrderByPriorityFee::default(),
		),
	);

	let node = P::create_test_node(pipeline).await.unwrap();
	pool.attach_to_test_node(&node).unwrap();

	let mut transfers = vec![];
	for i in 0..10 {
		let tx = node.build_tx().transfer().with_value(U256::from(i + 1));
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
		block.includes(&reverts),
		"Block should not include any reverts"
	);

	assert_eq!(block.header().number(), 1);

	if_platform!(Ethereum => {
		assert_eq!(block.tx_count(), transfers.len() + reverts.len());
	});

	if_platform!(Optimism => {
		// Optimism should include an extra sequencer deposit transaction
		assert_eq!(block.tx_count(), transfers.len() + reverts.len() + 1);
	});
}

#[tokio::test]
#[ignore = "This test never completes but ensures that this syntax compiles"]
#[allow(clippy::large_futures)]
async fn reth_minimal_integration_example() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::<Ethereum>::default().with_pipeline(
		Loop,
		(
			AppendOrders::from_pool(&pool),
			OrderByPriorityFee::default(),
			OrderByCoinbaseProfit::default(),
			RemoveRevertedTransactions::default(),
		),
	);

	Cli::parse_args()
		.run(|builder, _| async move {
			let handle = builder
				.with_types::<EthereumNode>()
				.with_components(
					EthereumNode::components().payload(pipeline.into_service()),
				)
				.with_add_ons(EthereumAddOns::default())
				.launch()
				.await?;

			handle.wait_for_node_exit().await
		})
		.unwrap();
}
