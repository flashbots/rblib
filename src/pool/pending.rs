use {
	crate::{
		alloy::consensus::Transaction,
		alloy::primitives::{Address, B256},
		pool::{Order, Platform},
	},
	std::collections::{HashMap, HashSet, VecDeque},
	tracing::warn,
};

type NonceBitmap = u64;
const BITMAP_BITS: u64 = core::mem::size_of::<NonceBitmap>() as u64 * 8;

#[derive(Debug, Default, Clone, Copy)]
struct KnownNonces {
	// Next nonce known to be valid
	expected: u64,
	/// Bitmap of relative seen nonces
	seen_relative: NonceBitmap,
}

/// Implements a holding place for orders arriving in the wrong order
#[derive(Default, Debug)]
pub struct PendingOrders<P: Platform> {
	nonces: HashMap<Address, KnownNonces>,
	blocked: HashMap<B256, Order<P>>,
	blocked_on: HashMap<Address, B256>,
	outbox: VecDeque<Order<P>>,
}

impl<P: Platform> PendingOrders<P> {
	pub fn push(&mut self, order: Order<P>) {
		let mut unresolved = HashSet::new();
		let mut by_addr = HashMap::new();

		for transaction in order.transactions() {
			let addr = transaction.signer();
			let nonce = transaction.nonce();

			// state order: local, global, default
			let KnownNonces {
				expected,
				seen_relative,
			} = by_addr
				.entry(addr)
				.or_insert_with(|| self.nonces.get(&addr).cloned().unwrap_or_default());

			if nonce < *expected {
				// repeated nonce, always invalid
				return;
			}

			let relative = nonce - *expected;

			if relative == 0 {
				*seen_relative >>= 1;
				let filled = seen_relative.trailing_ones() as u64;
				*expected += filled + 1;
				*seen_relative >>= filled;
			} else if relative < BITMAP_BITS {
				*seen_relative |= 1 << relative;
			} else {
				warn!("Dropping order: nonce gap {relative} >= {BITMAP_BITS}");
				return;
			}

			if *seen_relative == 0 {
				unresolved.remove(&addr);
			} else {
				unresolved.insert(addr);
			}
		}

		if unresolved.is_empty() {
			self.outbox.push_back(order);
			for (addr, known) in by_addr.drain() {
				self.nonces.insert(addr, known);
				self
					.blocked_on
					.remove(&addr)
					.map(|hash| self.blocked.remove(&hash).map(|order| self.push(order)));
			}
		} else {
			let order_hash = order.hash();
			for addr in unresolved.drain() {
				self.blocked_on.insert(addr, order_hash);
			}
			self.blocked.insert(order_hash, order);
		}
	}

	pub fn pop(&mut self) -> Option<Order<P>> {
		self.outbox.pop_front()
	}
}

#[cfg(test)]
mod test {
	use {
		crate::pool::{FlashbotsBundle, Order, OrderPool},
		crate::prelude::{BlockContext, Optimism},
		crate::test_utils::PrivateKeySignerOpExt,
		crate::test_utils::*,
	};

	fn mock_block() -> BlockContext<Optimism> {
		BlockContext::mocked().0
	}

	#[test]
	fn test_pool_iterator_valid() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(1));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 1);
	}

	#[test]
	fn test_pool_iterator_invalid() {
		let op = OrderPool::default();

		// two transactions with the same nonce!
		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(0));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 0);
	}

	#[test]
	fn test_pool_iterator_single_bundle_reverse() {
		let op = OrderPool::default();

		// two transactions in reverse order

		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2))
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 1);
	}

	#[test]
	fn test_pool_iterator_single_bundle_alternating() {
		let op = OrderPool::default();

		// two transactions in reverse order

		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 1);
	}

	#[test]
	fn test_pool_iterator_invalid_nonce_gap() {
		let op = OrderPool::default();

		// two transactions in reverse order
		let account = FundedAccounts::signer(0);

		let bundle = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 0);
	}

	#[test]
	fn test_pool_iterator_multiple_correct_order_a() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(1));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(2))
			.with_transaction(account.test_tx_with_nonce(3));

		op.insert(Order::Bundle(bundle_a));
		op.insert(Order::Bundle(bundle_b));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}

	#[test]
	fn test_pool_iterator_multiple_correct_order_b() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle_a));
		op.insert(Order::Bundle(bundle_b));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}

	#[test]
	fn test_pool_iterator_multiple_reverse_order_a() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(0))
			.with_transaction(account.test_tx_with_nonce(1));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(2))
			.with_transaction(account.test_tx_with_nonce(3));

		op.insert(Order::Bundle(bundle_b));
		op.insert(Order::Bundle(bundle_a));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}

	#[test]
	fn test_pool_iterator_multiple_reverse_order_b() {
		let op = OrderPool::default();

		let account = FundedAccounts::signer(0);

		let bundle_a = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(1))
			.with_transaction(account.test_tx_with_nonce(0));

		let bundle_b = FlashbotsBundle::default()
			.with_transaction(account.test_tx_with_nonce(3))
			.with_transaction(account.test_tx_with_nonce(2));

		op.insert(Order::Bundle(bundle_b));
		op.insert(Order::Bundle(bundle_a));

		assert_eq!(op.best_orders_for_block(&mock_block()).count(), 2);
	}
}
