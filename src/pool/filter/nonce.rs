use {
	super::*,
	crate::{alloy, reth},
	alloy::consensus::Transaction,
	reth::primitives::SealedHeader,
	std::collections::{HashMap, hash_map::Entry},
};

#[derive(Default, Debug)]
pub struct NonceFilter;

impl<P: Platform> OrderFilter<P> for NonceFilter {
	/// Bundles are intrinsically ineligible if they have nonce gaps in
	/// transactions from the same signer.
	fn intrinsic(&self, order: &Order<P>) -> bool {
		if let Order::Bundle(bundle) = order {
			let mut by_signer = HashMap::new();
			for tx in bundle.transactions() {
				if tx.nonce() == u64::MAX {
					return false; // EIP-2681
				}

				match by_signer.entry(tx.signer()) {
					Entry::Occupied(occupied_entry) => {
						if tx.nonce() != occupied_entry.get() + 1 {
							return false; // Nonce gap for this signer
						}
						*occupied_entry.into_mut() = tx.nonce();
					}
					Entry::Vacant(vacant_entry) => {
						vacant_entry.insert(tx.nonce());
					}
				}
			}
		} else if let Order::Transaction(tx) = order {
			if tx.nonce() == u64::MAX {
				return false; // EIP-2681
			}
		}

		true
	}

	/// An order is globally ineligible if any of its transactions have a
	/// nonce lower than the current account nonce.
	fn global(
		&self,
		state: &dyn StateProvider,
		_: &SealedHeader<types::Header<P>>,
		order: &Order<P>,
	) -> eyre::Result<Eligibility> {
		// TODO: Revisit this logic for bundles with multiple txs from the same
		// signer.
		for (signer, tx_nonce) in order.nonces() {
			let acc_nonce = state.account_nonce(&signer)?.unwrap_or_default();

			if tx_nonce < acc_nonce {
				return Ok(Eligibility::PermanentlyIneligible);
			} else if tx_nonce > acc_nonce {
				return Ok(Eligibility::TemporarilyIneligible);
			}
		}

		Ok(Eligibility::Eligible)
	}

	fn block(
		&self,
		block: &BlockContext<P>,
		order: &Order<P>,
	) -> eyre::Result<Eligibility> {
		self.global(block.base_state(), block.parent(), order)
	}

	fn checkpoint(
		&self,
		checkpoint: &Checkpoint<P>,
		order: &Order<P>,
	) -> eyre::Result<Eligibility> {
		for (signer, tx_nonce) in order.nonces() {
			let acc_nonce = checkpoint.nonce_of(signer)?;

			if tx_nonce < acc_nonce {
				return Ok(Eligibility::PermanentlyIneligible);
			} else if tx_nonce > acc_nonce {
				return Ok(Eligibility::TemporarilyIneligible);
			}
		}

		Ok(Eligibility::Eligible)
	}
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::test_utils::*,
		alloy::{
			consensus::SignableTransaction,
			network::{TransactionBuilder, TxSignerSync},
			primitives::{Address, U256},
			signers::local::PrivateKeySigner,
		},
		reth::{ethereum::primitives::SignedTransaction, primitives::Recovered},
	};

	#[rblib_test(Ethereum, Optimism)]
	fn individual_tx_intrinsic<P: TestablePlatform>() {
		let filter = NonceFilter;

		let tx = make_tx::<P>(&FundedAccounts::signer(0), 0);
		assert!(filter.intrinsic(&Order::<P>::Transaction(tx)));

		let tx = make_tx::<P>(&FundedAccounts::signer(0), u64::MAX);
		assert!(!filter.intrinsic(&Order::<P>::Transaction(tx)));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn bundle_with_one_tx_intrinsic<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let filter = NonceFilter;

		let tx = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let bundle = FlashbotsBundle::default().with_transaction(tx);
		assert!(OrderFilter::<P>::intrinsic(&filter, &Order::Bundle(bundle)));

		let tx = make_tx::<P>(&FundedAccounts::signer(0), u64::MAX);
		let bundle = FlashbotsBundle::default().with_transaction(tx);
		assert!(!OrderFilter::<P>::intrinsic(
			&filter,
			&Order::Bundle(bundle)
		));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn bundle_nonce_gap_intrinsic<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let filter = NonceFilter;

		// no gap, should be fine
		let tx1 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(0), 1);
		let bundle = FlashbotsBundle::default()
			.with_transaction(tx1)
			.with_transaction(tx2);
		assert!(OrderFilter::<P>::intrinsic(&filter, &Order::Bundle(bundle)));

		// two signers, no gaps
		let tx1 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(0), 1);
		let tx3 = make_tx::<P>(&FundedAccounts::signer(1), 1);
		let tx4 = make_tx::<P>(&FundedAccounts::signer(1), 2);
		let bundle = FlashbotsBundle::default()
			.with_transaction(tx1)
			.with_transaction(tx2)
			.with_transaction(tx3)
			.with_transaction(tx4);
		assert!(OrderFilter::<P>::intrinsic(&filter, &Order::Bundle(bundle)));

		// one signer, with gap
		let tx1 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(0), 2);
		let bundle = FlashbotsBundle::default()
			.with_transaction(tx1)
			.with_transaction(tx2);
		assert!(!OrderFilter::<P>::intrinsic(
			&filter,
			&Order::Bundle(bundle)
		));

		// two signers, one with gap
		let tx1 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(0), 1);
		let tx3 = make_tx::<P>(&FundedAccounts::signer(1), 0);
		let tx4 = make_tx::<P>(&FundedAccounts::signer(1), 2);
		let bundle = FlashbotsBundle::default()
			.with_transaction(tx1)
			.with_transaction(tx2)
			.with_transaction(tx3)
			.with_transaction(tx4);
		assert!(!OrderFilter::<P>::intrinsic(
			&filter,
			&Order::Bundle(bundle)
		));
	}

	fn make_tx<P: PlatformWithRpcTypes>(
		signer: &PrivateKeySigner,
		nonce: u64,
	) -> Recovered<types::Transaction<P>> {
		let mut tx = types::TransactionRequest::<P>::default()
			.with_nonce(nonce)
			.with_to(Address::random())
			.with_value(U256::from(1_000u64))
			.with_gas_price(1_000_000_000)
			.with_gas_limit(21_000)
			.with_max_priority_fee_per_gas(1_000_000)
			.with_max_fee_per_gas(2_000_000)
			.build_unsigned()
			.expect("valid transaction request");

		let sig = signer
			.sign_transaction_sync(&mut tx)
			.expect("signing should succeed");

		let envelope: types::TxEnvelope<P> = tx.into_signed(sig).into();
		let tx: types::Transaction<P> = envelope.into();
		tx.try_into_recovered().unwrap()
	}
}
