use {
	super::{OrderBy, OrderScore},
	crate::{alloy, prelude::*},
	alloy::primitives::U256,
	core::marker::PhantomData,
	reth_errors::ProviderError,
};

#[derive(Debug, Clone, Default)]
pub struct CoinbaseProfitScore<P: Platform>(PhantomData<P>);

impl<P: Platform> OrderScore<P> for CoinbaseProfitScore<P> {
	type Error = ProviderError;
	type Score = U256;

	fn score(checkpoint: &Checkpoint<P>) -> Result<Self::Score, Self::Error> {
		let fee_recipient = checkpoint.block().coinbase();
		let current_balance = checkpoint.balance_of(fee_recipient)?;

		let prev_balance = checkpoint
			.prev()
			.unwrap_or_else(|| checkpoint.root())
			.balance_of(fee_recipient)?;

		Ok(current_balance.saturating_sub(prev_balance))
	}
}

/// This step will sort the checkpoints in the staging part of the payload by
/// the difference in the coinbase account balance before and after they are
/// executed.
pub type OrderByCoinbaseProfit<P: Platform> =
	OrderBy<P, CoinbaseProfitScore<P>>;

#[cfg(test)]
mod tests {
	// NOTE: Here we are for now reusing the tests from `OrderByPriorityFee` since
	// the priority fee goes to the coinbase account as well and the tests are
	// simple transfers that do not change the coinbase account balance in other
	// ways.
	//
	// TODO: Expand those tests to cover contract calls and others.

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
		let mut step = OneStep::<P>::new(OrderByCoinbaseProfit::default());

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

	#[rblib_test(Ethereum, Optimism)]
	async fn explicit_transfer_to_coinbase<P: TestablePlatform>() {
		let mut step = OneStep::<P>::new(OrderByCoinbaseProfit::default());

		// create a payload that has three transfers, two of them transfer to random
		// addresses and have high priority fees, and one that has low priority fee
		// but explicitly transfers to the coinbase address more than the other
		// two generate in fees.
		step = step
			.with_payload_tx(move |b| {
				b.transfer()
					.with_nonce(0)
					.with_funded_signer(0)
					.with_value(U256::from(1000))
					.with_max_priority_fee_per_gas(1000)
			})
			.with_payload_tx(move |b| {
				b.transfer()
					.with_nonce(0)
					.with_funded_signer(1)
					.with_value(U256::from(1000))
					.with_max_priority_fee_per_gas(2000)
			})
			.with_payload_tx(move |b| {
				b.transfer()
					.with_nonce(0)
					.with_funded_signer(2)
					.with_value(U256::from(500_000_000_000u64))
					.with_to(TEST_COINBASE)
					.with_max_priority_fee_per_gas(10)
			});

		let output = step.run().await;
		let Ok(ControlFlow::Ok(payload)) = output else {
			panic!("Expected Ok payload, got: {output:?}");
		};

		let history = payload.history();
		let txs = history.transactions().collect::<Vec<_>>();

		// The transaction with the lowest priority fee actually generates the most
		// profit for the coinbase account and should be first in the
		// ordering, the remaining two should be ordered by their priority fee.
		assert_eq!(txs.len(), 3);
		assert_eq!(txs[0].signer(), FundedAccounts::address(2));
		assert_eq!(txs[1].signer(), FundedAccounts::address(1));
		assert_eq!(txs[2].signer(), FundedAccounts::address(0));
	}
}
