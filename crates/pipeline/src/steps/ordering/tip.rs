use {
	super::{OrderBy, OrderScore},
	crate::prelude::*,
	core::{convert::Infallible, marker::PhantomData},
};

#[derive(Debug, Clone, Default)]
pub struct PriorityFeeScore<P: Platform>(PhantomData<P>);

impl<P: Platform> OrderScore<P> for PriorityFeeScore<P> {
	type Error = Infallible;
	type Score = u128;

	fn score(checkpoint: &Checkpoint<P>) -> Result<Self::Score, Self::Error> {
		Ok(checkpoint.effective_tip_per_gas())
	}
}

/// This step will sort the checkpoints in the payload by their effective
/// priority fee. During the sorting the transactions will preserve their
/// sender, nonce dependencies.
///
/// Sorting happens only for the staging part of the payload, i.e. after
/// the last barrier checkpoint. Anything prior to the last barrier
/// checkpoint is considered immutable and will not be reordered.
pub type OrderByPriorityFee<P: Platform> = OrderBy<P, PriorityFeeScore<P>>;

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::{
			alloy::{consensus::Transaction, network::TransactionBuilder},
			test_utils::*,
		},
	};

	async fn test_ordering<P: TestablePlatform>(
		payload: Vec<(u32, u64, u128)>, // signer_id, nonce, tip
		expected: Vec<(u32, u64, u128)>,
	) {
		let mut step = OneStep::<P>::new(OrderByPriorityFee::default());

		for (sender_id, nonce, tip) in payload {
			if sender_id == u32::MAX && nonce == u64::MAX && tip == u128::MAX {
				// this is a barrier
				step = step.with_payload_barrier();
			} else {
				// this is a transaction
				step = step.with_payload_tx(move |b| {
					b.transfer()
						.with_funded_signer(sender_id)
						.with_nonce(nonce)
						.with_max_priority_fee_per_gas(tip)
				});
			}
		}

		let output = step.run().await;
		let Ok(ControlFlow::Ok(payload)) = output else {
			panic!("Expected Ok payload, got: {output:?}");
		};

		let history = payload.history();
		let txs = history.transactions().collect::<Vec<_>>();

		let actual = txs
			.into_iter()
			.map(|tx| {
				(
					FundedAccounts::index(tx.signer()).unwrap(),
					tx.nonce(),
					tx.max_priority_fee_per_gas().unwrap_or_default(),
				)
			})
			.collect::<Vec<_>>();

		assert_eq!(actual, expected);
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn empty_payload<P: TestablePlatform>() {
		let payload = vec![];
		test_ordering::<P>(payload.clone(), payload).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn correct_order_remains_unchanged_nonce_deps<P: TestablePlatform>() {
		let payload = vec![
			(1, 0, 170),
			(2, 0, 160),
			(2, 1, 155),
			(1, 1, 150),
			(1, 2, 190),
			(1, 3, 160),
			(2, 2, 120),
			(2, 3, 170),
		];

		test_ordering::<P>(payload.clone(), payload).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn correct_order_remains_unchanged_no_nonce_deps<
		P: TestablePlatform,
	>() {
		let payload = vec![
			(1, 0, 170),
			(2, 0, 160),
			(3, 0, 155),
			(4, 0, 150),
			(5, 0, 140),
			(6, 0, 130),
			(7, 0, 120),
			(8, 0, 110),
		];

		test_ordering::<P>(payload.clone(), payload).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn everything_unordered_no_nonce_deps<P: TestablePlatform>() {
		let payload = vec![
			(1, 0, 170),
			(2, 0, 150),
			(3, 0, 160),
			(4, 0, 120),
			(5, 0, 175),
			(6, 0, 190),
			(7, 0, 155),
			(8, 0, 140),
		];

		let expected = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(3, 0, 160),
			(7, 0, 155),
			(2, 0, 150),
			(8, 0, 140),
			(4, 0, 120),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn everything_unordered_no_nonce_deps_with_barrier<
		P: TestablePlatform,
	>() {
		let payload = vec![
			(1, 0, 170),
			(2, 0, 150),
			(3, 0, 160),
			(u32::MAX, u64::MAX, u128::MAX), // barrier
			(4, 0, 120),
			(5, 0, 175),
			(6, 0, 190),
			(7, 0, 155),
			(8, 0, 140),
		];

		let expected = vec![
			(1, 0, 170),
			(2, 0, 150),
			(3, 0, 160),
			(6, 0, 190),
			(5, 0, 175),
			(7, 0, 155),
			(8, 0, 140),
			(4, 0, 120),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn everything_unordered_no_nonce_deps_with_two_barriers<
		P: TestablePlatform,
	>() {
		let payload = vec![
			(1, 0, 170),
			(2, 0, 150),
			(3, 0, 160),
			(u32::MAX, u64::MAX, u128::MAX), // barrier
			(4, 0, 120),
			(5, 0, 175),
			(6, 0, 190),
			(u32::MAX, u64::MAX, u128::MAX), // barrier
			(8, 0, 140),
			(7, 0, 155),
		];

		let expected = vec![
			(1, 0, 170),
			(2, 0, 150),
			(3, 0, 160),
			(4, 0, 120),
			(5, 0, 175),
			(6, 0, 190),
			(7, 0, 155),
			(8, 0, 140),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn everything_unordered_nonce_deps<P: TestablePlatform>() {
		let payload = vec![
			(1, 0, 170),
			(2, 0, 150),
			(2, 1, 160),
			(1, 1, 120),
			(5, 0, 175),
			(6, 0, 190),
			(7, 0, 155),
			(8, 0, 140),
		];

		let expected = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(7, 0, 155),
			(2, 0, 150),
			(2, 1, 160),
			(8, 0, 140),
			(1, 1, 120),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn everything_unordered_nonce_deps_with_barrier<P: TestablePlatform>() {
		let payload = vec![
			(1, 0, 170),
			(2, 0, 150),
			(u32::MAX, u64::MAX, u128::MAX), // barrier
			(2, 1, 160),
			(1, 1, 120),
			(5, 0, 175),
			(6, 0, 190),
			(7, 0, 155),
			(8, 0, 140),
		];

		let expected = vec![
			(1, 0, 170),
			(2, 0, 150),
			(6, 0, 190),
			(5, 0, 175),
			(2, 1, 160),
			(7, 0, 155),
			(8, 0, 140),
			(1, 1, 120),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn partially_unordered_no_nonce_deps<P: TestablePlatform>() {
		let payload = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(2, 0, 150),
			(3, 0, 160),
			(4, 0, 120),
			(7, 0, 155),
			(8, 0, 140),
		];
		let expected = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(3, 0, 160),
			(7, 0, 155),
			(2, 0, 150),
			(8, 0, 140),
			(4, 0, 120),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn partially_unordered_nonce_deps<P: TestablePlatform>() {
		let payload = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(2, 0, 150),
			(3, 0, 160),
			(4, 0, 120),
			(4, 1, 155),
			(8, 0, 140),
		];
		let expected = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(3, 0, 160),
			(2, 0, 150),
			(8, 0, 140),
			(4, 0, 120),
			(4, 1, 155),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn only_last_pair_unordered_no_nonce_deps<P: TestablePlatform>() {
		let payload = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(3, 0, 160),
			(7, 0, 155),
			(2, 0, 150),
			(4, 0, 120),
			(8, 0, 140),
		];
		let expected = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(3, 0, 160),
			(7, 0, 155),
			(2, 0, 150),
			(8, 0, 140),
			(4, 0, 120),
		];

		test_ordering::<P>(payload, expected).await;
	}

	#[rblib_test(Ethereum, Optimism)]
	async fn only_last_pair_unordered_nonce_deps<P: TestablePlatform>() {
		let payload = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(3, 0, 160),
			(7, 0, 155),
			(2, 0, 150),
			(4, 0, 120),
			(4, 1, 140),
		];
		let expected = vec![
			(6, 0, 190),
			(5, 0, 175),
			(1, 0, 170),
			(3, 0, 160),
			(7, 0, 155),
			(2, 0, 150),
			(4, 0, 120),
			(4, 1, 140),
		];

		test_ordering::<P>(payload, expected).await;
	}
}
