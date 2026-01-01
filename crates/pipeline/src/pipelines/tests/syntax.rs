use {
	crate::{alloy, prelude::*, steps::*, test_utils::*},
	alloy::signers::local::LocalSigner,
	tracing::info,
};

fake_step!(AppendOrdersMock);

#[test]
fn only_steps() {
	let pipeline = Pipeline::<Ethereum>::default()
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()))
		.with_step(AppendOrdersMock)
		.with_step(OrderByPriorityFee::default())
		.with_step(RemoveRevertedTransactions::default());

	info!("{pipeline:#?}");
}

#[test]
#[cfg(feature = "optimism")]
fn only_steps_optimism_specific() {
	use tracing::info;

	let pipeline = Pipeline::<Optimism>::default()
		.with_prologue(OptimismPrologue)
		.with_step(AppendOrdersMock)
		.with_step(OrderByPriorityFee::default())
		.with_step(OrderByCoinbaseProfit::default())
		.with_step(RemoveRevertedTransactions::default())
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()));

	info!("{pipeline:#?}");
}

#[test]
fn nested_verbose() {
	let top_level = Pipeline::<Ethereum>::default()
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()));

	let nested = Pipeline::<Ethereum>::default()
		.with_step(AppendOrdersMock)
		.with_step(OrderByPriorityFee::default())
		.with_step(OrderByCoinbaseProfit::default())
		.with_step(RemoveRevertedTransactions::default());

	let top_level = top_level //
		.with_pipeline(Loop, nested);

	info!("{top_level:#?}");
}

#[test]
fn nested_one_concise() {
	let top_level = Pipeline::<Ethereum>::default()
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()))
		.with_pipeline(Loop, (AppendOrdersMock,));

	info!("{top_level:#?}");
}

#[test]
#[cfg(feature = "optimism")]
fn nested_many_concise() {
	// synthesize dummy steps
	fake_step!(TestStep1);
	fake_step!(TestStep2);

	let top_level = Pipeline::<Optimism>::default()
		.with_prologue(OptimismPrologue)
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()))
		.with_step(TestStep1)
		.with_pipeline(
			Loop,
			(
				AppendOrdersMock,
				OrderByPriorityFee::default(),
				OrderByCoinbaseProfit::default(),
				RemoveRevertedTransactions::default(),
			),
		)
		.with_step(TestStep2);

	info!("{top_level:#?}");
}

#[test]
#[allow(dead_code)]
#[cfg(feature = "optimism")]
fn flashblocks_example_closure() {
	use core::time::Duration;

	#[derive(Debug, Clone)]
	struct FlashblocksConfig {
		count: usize,
		interval: Duration,
	}

	fake_step!(WebSocketBeginBlock);
	fake_step!(WebSocketEndBlock);
	fake_step!(FlashblockEpilogue);
	fake_step!(PublishToWebSocket, FlashblocksConfig);

	#[derive(Debug)]
	struct FlashblockLimits(FlashblocksConfig);
	impl<P: Platform> ScopedLimits<P> for FlashblockLimits {
		fn create(&self, _: &Checkpoint<P>, _: &Limits<P>) -> Limits<P> {
			unimplemented!()
		}
	}

	let config = FlashblocksConfig {
		count: 5,
		interval: Duration::from_millis(200),
	};

	let pipeline = Pipeline::<Optimism>::default()
		.with_prologue(OptimismPrologue)
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()))
		.with_step(WebSocketBeginBlock)
		.with_pipeline(Loop, |nested: Pipeline<Optimism>| {
			nested
				.with_limits(FlashblockLimits(config.clone()))
				.with_epilogue(FlashblockEpilogue)
				.with_pipeline(
					Loop,
					(
						AppendOrdersMock,
						OrderByPriorityFee::default(),
						OrderByCoinbaseProfit::default(),
						RemoveRevertedTransactions::default(),
					),
				)
				.with_step(PublishToWebSocket(config))
		})
		.with_step(WebSocketEndBlock);

	info!("{pipeline:#?}");
}

#[tokio::test]
#[allow(dead_code)]
#[cfg(feature = "optimism")]
async fn flashblocks_example_concise_fractional() {
	use core::num::NonZero;

	#[derive(Debug, Clone)]
	struct FlashblocksConfig {
		count: NonZero<u32>,
	}

	fake_step!(WebSocketBeginBlock);
	fake_step!(WebSocketEndBlock);
	fake_step!(FlashblockEpilogue);
	fake_step!(PublishToWebSocket, FlashblocksConfig);

	let config = FlashblocksConfig {
		count: NonZero::new(5).unwrap(),
	};

	let pipeline = Pipeline::<Optimism>::default() // deadline 1
		.with_prologue(OptimismPrologue)
		.with_step(WebSocketBeginBlock)
		.with_pipeline(Loop, |nested: Pipeline<Optimism>| {
			nested
				.with_pipeline(
					Loop,
					(
						AppendOrdersMock,
						OrderByPriorityFee::default(),
						OrderByCoinbaseProfit::default(),
						RemoveRevertedTransactions::default(),
					)
						.with_limits(
							Scaled::new()
								.deadline(Fraction(1, config.count))
								.gas(Fraction(1, config.count)),
						)
						.with_epilogue(FlashblockEpilogue), // deadline 2, derived from 1
				)
				.with_step(PublishToWebSocket(config))
		})
		.with_step(WebSocketEndBlock)
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()));

	info!("{pipeline:#?}");
}

#[tokio::test]
#[allow(dead_code)]
#[cfg(feature = "optimism")]
async fn flashblocks_example_concise_fixed_interval() {
	use core::{num::NonZero, time::Duration};

	#[derive(Debug, Clone)]
	struct FlashblocksConfig {
		block_interval: Duration,
		max_gas_per_block: Fraction,
	}

	fake_step!(WebSocketBeginBlock);
	fake_step!(WebSocketEndBlock);
	fake_step!(FlashblockEpilogue);
	fake_step!(PublishToWebSocket, FlashblocksConfig);

	let config = FlashblocksConfig {
		block_interval: Duration::from_millis(250),
		max_gas_per_block: Fraction(1, NonZero::new(3).unwrap()),
	};

	let pipeline = Pipeline::<Optimism>::default() // deadline 1
		.with_prologue(OptimismPrologue)
		.with_step(WebSocketBeginBlock)
		.with_pipeline(Loop, |nested: Pipeline<Optimism>| {
			nested
				.with_pipeline(
					Loop,
					(
						AppendOrdersMock,
						OrderByPriorityFee::default(),
						RemoveRevertedTransactions::default(),
					)
						.with_limits(
							Scaled::new()
								.deadline(Fixed(config.block_interval))
								.gas(config.max_gas_per_block),
						)
						.with_epilogue(FlashblockEpilogue), // deadline 2, derived from 1
				)
				.with_step(PublishToWebSocket(config))
		})
		.with_step(WebSocketEndBlock)
		.with_epilogue(BuilderEpilogue::with_signer(LocalSigner::random()));

	info!("{pipeline:#?}");
}
