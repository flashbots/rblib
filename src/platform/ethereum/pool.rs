use {
	crate::{
		alloy::{consensus::Transaction, primitives::map::foldhash::HashMap},
		prelude::*,
		reth::{
			primitives::Recovered,
			transaction_pool::{
				BestTransactions,
				EthPooledTransaction,
				PoolTransaction,
				TransactionOrigin,
				ValidPoolTransaction,
				error::InvalidPoolTransactionError,
				identifier::{SenderId, SenderIdentifiers, TransactionId},
			},
		},
	},
	std::{collections::hash_map::Entry, sync::Arc},
};

pub struct FixedTransactions<P: Platform> {
	txs: Vec<Recovered<types::Transaction<P>>>,
	senders: SenderIdentifiers,
	invalid: HashMap<SenderId, TransactionId>,
}

impl<P: Platform> FixedTransactions<P> {
	pub fn new(txs: Vec<Recovered<types::Transaction<P>>>) -> Self {
		// reverse because we want to pop from the end
		// in the iterator.
		let mut txs = txs;
		txs.reverse();

		Self {
			txs,
			senders: SenderIdentifiers::default(),
			invalid: HashMap::default(),
		}
	}
}

impl<P: Platform> BestTransactions for FixedTransactions<P>
where
	EthPooledTransaction<types::Transaction<P>>:
		PoolTransaction<Consensus = types::Transaction<P>>,
{
	fn no_updates(&mut self) {}

	fn set_skip_blobs(&mut self, _: bool) {}

	fn mark_invalid(&mut self, tx: &Self::Item, _: InvalidPoolTransactionError) {
		match self.invalid.entry(tx.transaction_id.sender) {
			Entry::Vacant(e) => {
				e.insert(tx.transaction_id);
			}
			Entry::Occupied(mut e) => {
				if e.get().nonce < tx.transaction_id.nonce {
					e.insert(tx.transaction_id);
				}
			}
		}
	}
}

impl<P: Platform> Iterator for FixedTransactions<P>
where
	EthPooledTransaction<types::Transaction<P>>:
		PoolTransaction<Consensus = types::Transaction<P>>,
{
	type Item =
		Arc<ValidPoolTransaction<EthPooledTransaction<types::Transaction<P>>>>;

	fn next(&mut self) -> Option<Self::Item> {
		loop {
			let transaction = self.txs.pop()?;

			let Ok(pooled) =
				EthPooledTransaction::<types::Transaction<P>>::try_from_consensus(
					transaction,
				)
			else {
				unreachable!("Transaction should be valid at this point");
			};

			let nonce = pooled.nonce();
			let sender_id = self.senders.sender_id_or_create(pooled.sender());

			if let Some(id) = self.invalid.get(&sender_id) {
				if id.nonce <= nonce {
					// transaction or one of its ancestors is marked as invalid, skip it
					continue;
				}
			}

			let wrapper = ValidPoolTransaction {
				transaction: pooled,
				transaction_id: TransactionId::new(sender_id, nonce),
				propagate: false,
				timestamp: std::time::Instant::now(),
				origin: TransactionOrigin::Private,
				authority_ids: None,
			};

			return Some(Arc::new(wrapper));
		}
	}
}
