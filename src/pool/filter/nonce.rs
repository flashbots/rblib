use {super::*, alloy::consensus::Transaction};

#[derive(Default, Debug)]
pub struct NonceFilter;

impl<P: Platform> OrderFilter<P> for NonceFilter {
	/// Reject orders that will never be eligible
	fn global(
		&self,
		state: &dyn StateProvider,
		_: &SealedHeader<types::Header<P>>,
		order: &Order<P>,
	) -> eyre::Result<Eligibility> {
		match order {
			Order::Transaction(tx) => {
				let tx_nonce = tx.nonce();
				let acc_nonce = state.account_nonce(&tx.signer())?.unwrap_or_default();

				if tx_nonce < acc_nonce {
					return Ok(Eligibility::PermanentlyIneligible);
				}

				Ok(Eligibility::Eligible)
			}
			Order::Bundle(bundle) => {
				for tx in bundle.transactions() {
					let tx_nonce = tx.nonce();
					let acc_nonce =
						state.account_nonce(&tx.signer())?.unwrap_or_default();

					if tx_nonce < acc_nonce {
						return Ok(Eligibility::PermanentlyIneligible);
					}
				}

				Ok(Eligibility::Eligible)
			}
		}
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
		match order {
			Order::Transaction(tx) => {
				let tx_nonce = tx.nonce();
				let acc_nonce = checkpoint.nonce_of(tx.signer())?;

				if tx_nonce < acc_nonce {
					return Ok(Eligibility::PermanentlyIneligible);
				} else if tx_nonce > acc_nonce {
					return Ok(Eligibility::TemporarilyIneligible);
				}

				Ok(Eligibility::Eligible)
			}
			Order::Bundle(bundle) => {
				for tx in bundle.transactions() {
					let tx_nonce = tx.nonce();
					let acc_nonce = checkpoint.nonce_of(tx.signer())?;

					if tx_nonce < acc_nonce {
						return Ok(Eligibility::PermanentlyIneligible);
					} else if tx_nonce > acc_nonce {
						return Ok(Eligibility::TemporarilyIneligible);
					}
				}
				Ok(Eligibility::Eligible)
			}
		}
	}
}
