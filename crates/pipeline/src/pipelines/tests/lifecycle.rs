use {
	crate::{
		alloy::{
			consensus::{BlockHeader, transaction::TxHashRef},
			network::BlockResponse,
			primitives::{B256, TxHash},
		},
		pool::{OrderPool, TxLifecycle, TxSource},
		prelude::*,
		reth::payload::builder::PayloadId,
		steps::*,
		test_utils::*,
	},
	core::time::Duration,
	tokio_stream::{StreamExt, wrappers::IntervalStream},
};

/// The lifecycle of every member of the bundle, all must be tracked.
fn member_lifecycles(
	pool: &OrderPool<Ethereum>,
	members: &[TxHash],
) -> Vec<TxLifecycle> {
	members
		.iter()
		.map(|tx| pool.lifecycle().get(tx).expect("member is tracked"))
		.collect()
}

/// A pool with a bundle of the given funded account inserted, the hash of the
/// bundle and the hashes of its members.
fn pool_with_bundle(key: u32) -> (OrderPool<Ethereum>, B256, Vec<TxHash>) {
	let pool = OrderPool::<Ethereum>::default();
	let (bundle, txs) = test_bundle::<Ethereum>(key, 0);
	let bundle_hash = bundle.hash();
	let members: Vec<TxHash> = txs.iter().map(|tx| *tx.tx_hash()).collect();
	pool.insert(Order::Bundle(bundle));
	(pool, bundle_hash, members)
}

/// The pipeline events listener polls the pipeline drop first (`biased`), so
/// events that a step emitted right before the drop are still buffered when
/// the drop is observed: the listener records them before it exits.
#[tokio::test]
async fn listener_records_the_events_buffered_before_the_pipeline_drop() {
	let payload_id = PayloadId::new([1, 0, 0, 0, 0, 0, 0, 0]);
	let (pool, bundle_hash, members) = pool_with_bundle(0);
	let pipeline = Pipeline::<Ethereum>::default();
	// the listener subscribes now: everything published below is buffered for
	// it until it is first polled
	let listener = pool.start_pipeline_events_listener(&pipeline);

	// the events of the last step and the drop are all ready on the first poll
	pipeline
		.events
		.publish(OrderInclusionAttempt(bundle_hash, payload_id));
	pipeline
		.events
		.publish(OrderInclusionSuccess(bundle_hash, payload_id));
	drop(pipeline);
	listener.await.expect("the listener exits cleanly");

	let lifecycles = member_lifecycles(&pool, &members);
	assert_eq!(lifecycles.len(), 3);
	assert!(lifecycles.iter().all(|lifecycle| {
		lifecycle.first_considered().map(|stage| stage.payload_id())
			== Some(payload_id)
			&& lifecycle.first_included().map(|stage| stage.payload_id())
				== Some(payload_id)
	}));
	// attempts are drained before successes: the stages keep emission order
	assert!(lifecycles.iter().all(|lifecycle| {
		lifecycle.first_considered().map(|stage| stage.at())
			<= lifecycle.first_included().map(|stage| stage.at())
	}));

	// negative row: a drop with nothing buffered records no stage
	let (pool, _bundle_hash, members) = pool_with_bundle(1);
	let pipeline = Pipeline::<Ethereum>::default();
	let listener = pool.start_pipeline_events_listener(&pipeline);
	drop(pipeline);
	listener.await.expect("the listener exits cleanly");

	let lifecycles = member_lifecycles(&pool, &members);
	assert_eq!(lifecycles.len(), 3);
	assert!(lifecycles.iter().all(|lifecycle| {
		lifecycle.first_considered().is_none()
			&& lifecycle.first_included().is_none()
	}));
}

/// The mined stage is recorded by the pool maintenance loop when the canonical
/// chain update reaches it, which happens asynchronously after the block is
/// built. Polls the lifecycle log until the transaction is marked as mined.
async fn wait_until_mined<P: Platform>(
	pool: &OrderPool<P>,
	tx_hash: &TxHash,
) -> TxLifecycle {
	const TIMEOUT: Duration = Duration::from_secs(5);
	const INTERVAL: Duration = Duration::from_millis(50);

	let mut mined = IntervalStream::new(tokio::time::interval(INTERVAL))
		.filter_map(|_| {
			pool
				.lifecycle()
				.get(tx_hash)
				.filter(|lifecycle| lifecycle.mined().is_some())
		});

	tokio::time::timeout(TIMEOUT, mined.next())
		.await
		.ok()
		.flatten()
		.expect("transaction should be marked as mined")
}

#[rblib_test(Ethereum, Optimism)]
async fn transaction_lifecycle_is_recorded_end_to_end<P: TestablePlatform>() {
	let pool = OrderPool::default();
	let pipeline = Pipeline::default().with_pipeline(
		Loop,
		(
			AppendOrders::from_pool(&pool),
			OrderByPriorityFee::default(),
		),
	);

	// listen to inclusion attempts and successes emitted by the pipeline
	pool.attach_pipeline(&pipeline);

	let node = P::create_test_node(pipeline).await.unwrap();
	pool.attach_to_test_node(&node).unwrap();

	let tx_hash = *node
		.send_tx(node.build_tx().transfer())
		.await
		.unwrap()
		.tx_hash();

	let block = node.next_block().await.unwrap();
	assert!(block.includes(tx_hash), "block should include the transfer");

	let lifecycle = wait_until_mined(&pool, &tx_hash).await;
	assert_eq!(lifecycle.source(), Some(TxSource::Mempool));
	assert!(lifecycle.first_considered().is_some());
	assert!(lifecycle.first_included().is_some());
	assert!(lifecycle.dropped().is_none());

	let mined = lifecycle.mined().expect("transaction should be mined");
	assert_eq!(mined.block_number(), block.header().number());
	assert_eq!(mined.block_timestamp(), block.header().timestamp());

	// stages happen in order
	assert!(
		lifecycle.time_to_first_consideration() <= lifecycle.time_to_inclusion()
	);
	assert!(lifecycle.time_to_inclusion() <= lifecycle.time_to_mined());
}
