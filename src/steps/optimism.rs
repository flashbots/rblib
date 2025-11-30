use {crate::prelude::*, ext::*, std::sync::Arc};

/// This step appends the sequencer transactions that are defined in the payload
/// attributes parameter from the CL node into the payload under construction.
/// It requires that the payload has no existing transactions in it as the
/// sequencer expects its transactions to be at the top of the block.
pub struct OptimismPrologue;
impl<P> Step<P> for OptimismPrologue
where
	P: traits::PlatformExecBounds<Optimism>,
{
	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		ctx: StepContext<P>,
	) -> ControlFlow<P> {
		let existing_transactions_count = payload.history().transactions().count();
		if existing_transactions_count > 0 {
			return ControlFlow::Fail(PayloadBuilderError::Other(
				format!(
					"Optimism sequencer transactions must be at the top of the block. \
					 This payload already has {existing_transactions_count} \
					 transaction(s).",
				)
				.into(),
			));
		}

		let mut gas_used = 0;
		let mut da_bytes_used = 0;

		// Apply all sequencer transactions to the payload.
		let mut payload = payload;
		for tx in &ctx.block().attributes().transactions {
			payload = match payload.apply(tx.clone().into_value()) {
				Ok(payload) => payload,
				Err(e) => {
					return PayloadBuilderError::Other(
						format!("Failed to apply sequencer transaction {tx:?}: {e:?}")
							.into(),
					)
					.into();
				}
			};

			// ensure that sequencer transactions do not exceed limits.
			// If this happens, fail the whole pipeline because we will never be able
			// to build a valid payload that fits the gas limit.
			gas_used += payload.gas_used();
			if gas_used > ctx.limits().gas_limit {
				return PayloadBuilderError::Other(
					format!(
						"Sequencer transactions exceed block gas limit: {gas_used} > {}",
						ctx.limits().gas_limit
					)
					.into(),
				)
				.into();
			}
			da_bytes_used += payload.da_bytes_used();
			if let Some(max_block_da) = ctx.limits().ext.max_block_da
				&& da_bytes_used > max_block_da
			{
				return PayloadBuilderError::Other(
					format!(
						"Sequencer transactions exceed data availability bytes limit: \
						 {da_bytes_used} > {max_block_da}",
					)
					.into(),
				)
				.into();
			}
		}

		// if there were sequencer transactions added to the payload, place a
		// barrier after them, so they won't be reordered or modified by subsequent
		// steps.
		if payload.depth() > 0 {
			payload = payload.barrier();
		}

		ControlFlow::Ok(payload)
	}
}

#[cfg(test)]
mod tests {
	use {
		crate::{prelude::*, steps::OptimismPrologue, test_utils::*},
		std::sync::Arc,
	};

	struct NoOpStep;
	impl Step<Optimism> for NoOpStep {
		async fn step(
			self: Arc<Self>,
			payload: Checkpoint<Optimism>,
			_: StepContext<Optimism>,
		) -> ControlFlow<Optimism> {
			ControlFlow::Ok(payload)
		}
	}

	#[tokio::test]
	async fn sequencer_txs_not_included_without_step() {
		let output = OneStep::new(NoOpStep).run().await.unwrap();

		let ControlFlow::Ok(payload) = output else {
			panic!("Expected Ok payload, got: {output:?}");
		};

		assert_eq!(payload.history().transactions().count(), 0);
		assert_eq!(payload.history_sealed().len(), 1); // only baseline checkpoint
		assert_eq!(payload.history_staging().len(), 1); // only last barrier
		assert_eq!(payload.history_staging().transactions().count(), 0);
	}

	#[tokio::test]
	async fn sequencer_txs_are_included() {
		let output = OneStep::<Optimism>::new(OptimismPrologue)
			.run()
			.await
			.unwrap();

		let ControlFlow::Ok(payload) = output else {
			panic!("Expected Ok payload, got: {output:?}");
		};

		assert_eq!(
			payload.history().transactions().count(),
			1,
			"sequencer transaction should be included"
		);

		assert!(
			payload.is_barrier(),
			"a barrier is placed after sequencer txs"
		);

		assert_eq!(
			payload.history_sealed().len(),
			3,
			"sealed history should have 3 checkpoints: baseline, sequencer tx and \
			 last barrier"
		);

		assert_eq!(
			payload.history_staging().len(),
			1,
			"staging history should have 1 checkpoint: last barrier"
		);

		let const_history = payload.history_sealed();
		let first_tx = const_history.transactions().next().unwrap();
		assert!(first_tx.is_deposit(), "Sequencer tx should be a deposit");
	}

	#[tokio::test]
	async fn fails_on_non_empty_payload() {
		let output = OneStep::<Optimism>::new(OptimismPrologue)
			.with_payload_tx(|tx| tx.transfer().with_default_signer().nonce(0))
			.run()
			.await
			.unwrap();

		let ControlFlow::Fail(err) = output else {
			panic!("Expected Fail, got: {output:?}");
		};

		assert!(err.to_string().contains(
			"Optimism sequencer transactions must be at the top of the block. This \
			 payload already has 1 transaction(s)."
		));
	}
}
