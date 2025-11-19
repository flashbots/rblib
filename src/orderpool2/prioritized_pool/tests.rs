use {
	super::*,
	crate::alloy::primitives::Address,
	std::{collections::HashMap, convert::Infallible},
};

struct PrioritizedOrderpoolHashMapNonces(HashMap<Address, u64>);

impl From<HashMap<Address, u64>> for PrioritizedOrderpoolHashMapNonces {
	fn from(map: HashMap<Address, u64>) -> Self {
		Self(map)
	}
}

impl OrderpoolNonceSource for PrioritizedOrderpoolHashMapNonces {
	type NonceError = Infallible;

	fn nonce(&self, address: &Address) -> Result<u64, Self::NonceError> {
		Ok(self.0.get(address).copied().unwrap_or_default())
	}
}

#[derive(Clone)]
pub struct PrioritizedOrderpoolTestBundle {
	id: u64,
	nonces: Vec<BundleNonce>,
	profit: u64,
}

impl OrderpoolOrder for PrioritizedOrderpoolTestBundle {
	type ID = u64;

	fn id(&self) -> Self::ID {
		self.id
	}

	fn nonces(&self) -> Vec<BundleNonce> {
		self.nonces.clone()
	}
}

type PrioritizedOrderpoolTestPriority = u64;

impl PrioritizedOrderpoolPriority for PrioritizedOrderpoolTestPriority {
	type Order = PrioritizedOrderpoolTestBundle;

	fn new(order: &Self::Order) -> Self {
		order.profit
	}
}

/// Helper struct for common `PrioritizedOrderStore` test operations
/// Works hardcoded on max profit ordering since it changes nothing on internal
/// logic
struct TestContext {
	generated_id_count: u64,
	nonces: PrioritizedOrderpoolHashMapNonces,
	order_pool: PrioritizedOrderpool<
		PrioritizedOrderpoolTestPriority,
		PrioritizedOrderpoolTestBundle,
	>,
}

impl TestContext {
	const ADDRESS1: Address = crate::alloy::primitives::address!(
		"0000000000000000000000000000000000000001"
	);
	const ADDRESS2: Address = crate::alloy::primitives::address!(
		"0000000000000000000000000000000000000002"
	);

	/// Context with 1 account to send txs from
	fn new_1_account(nonce: u64) -> (AccountNonce, Self) {
		let nonces = PrioritizedOrderpoolHashMapNonces(
			[(TestContext::ADDRESS1, nonce)].into_iter().collect(),
		);
		let account_nonce = AccountNonce {
			account: TestContext::ADDRESS1,
			nonce,
		};
		(account_nonce.clone(), TestContext {
			generated_id_count: 0,
			nonces,
			order_pool: PrioritizedOrderpool::default(),
		})
	}

	/// Context with 2 accounts to send txs from
	fn new_2_accounts(
		nonce_1: u64,
		nonce_2: u64,
	) -> (AccountNonce, AccountNonce, TestContext) {
		let nonces = PrioritizedOrderpoolHashMapNonces(
			[
				(TestContext::ADDRESS1, nonce_1),
				(TestContext::ADDRESS2, nonce_2),
			]
			.into_iter()
			.collect(),
		);
		let account_nonce_1 = AccountNonce {
			account: TestContext::ADDRESS1,
			nonce: nonce_1,
		};
		let account_nonce_2 = AccountNonce {
			account: TestContext::ADDRESS2,
			nonce: nonce_2,
		};
		(
			account_nonce_1.clone(),
			account_nonce_2.clone(),
			TestContext {
				generated_id_count: 0,
				nonces,
				order_pool: PrioritizedOrderpool::default(),
			},
		)
	}

	fn new_id(&mut self) -> u64 {
		let id = self.generated_id_count;
		self.generated_id_count += 1;
		id
	}

	fn create_add_tx_order(
		&mut self,
		tx_nonce: &AccountNonce,
		tx_profit: u64,
	) -> PrioritizedOrderpoolTestBundle {
		let order = PrioritizedOrderpoolTestBundle {
			id: self.new_id(),
			nonces: vec![BundleNonce {
				address: tx_nonce.account,
				nonce: tx_nonce.nonce,
				optional: false,
			}],
			profit: tx_profit,
		};
		self
			.order_pool
			.insert_order(order.clone(), &self.nonces)
			.unwrap();
		order
	}

	fn create_add_bundle_order_2_txs(
		&mut self,
		tx1_nonce: &AccountNonce,
		tx1_optional: bool,
		tx2_nonce: &AccountNonce,
		tx2_optional: bool,
		bundle_profit: u64,
	) -> PrioritizedOrderpoolTestBundle {
		let order = PrioritizedOrderpoolTestBundle {
			id: self.new_id(),
			nonces: vec![
				BundleNonce {
					address: tx1_nonce.account,
					nonce: tx1_nonce.nonce,
					optional: tx1_optional,
				},
				BundleNonce {
					address: tx2_nonce.account,
					nonce: tx2_nonce.nonce,
					optional: tx2_optional,
				},
			],
			profit: bundle_profit,
		};
		self
			.order_pool
			.insert_order(order.clone(), &self.nonces)
			.unwrap();
		order
	}

	fn update_nonce(&mut self, tx_nonce: &AccountNonce, new_nonce: u64) {
		self
			.order_pool
			.update_onchain_nonces(
				&[AccountNonce {
					account: tx_nonce.account,
					nonce: new_nonce,
				}],
				&self.nonces,
			)
			.unwrap();
	}

	fn assert_pop_order(&mut self, order: &PrioritizedOrderpoolTestBundle) {
		assert_eq!(
			self.order_pool.pop_order().map(|o| o.id()),
			Some(order.id())
		);
	}

	fn assert_pop_none(&mut self) {
		assert_eq!(self.order_pool.pop_order().map(|o| o.id()), None);
	}
}

#[test]
/// Tests 2 tx from different accounts, can execute both
fn test_block_orders_simple() {
	let (nonce_worst_order, nonce_best_order, mut context) =
		TestContext::new_2_accounts(0, 1);
	let worst_order = context.create_add_tx_order(&nonce_worst_order, 0);
	let best_order = context.create_add_tx_order(&nonce_best_order, 5);
	// we must see first the most profitable order
	context.assert_pop_order(&best_order);
	// we must see second the least profitable order
	context.assert_pop_order(&worst_order);
	// out of orders
	context.assert_pop_none();
}

#[test]
/// Tests 3 tx from the same account, only 1 can succeed
fn test_block_orders_competing_orders() {
	let (nonce, mut context) = TestContext::new_1_account(0);
	let middle_order = context.create_add_tx_order(&nonce, 3);
	let best_order = context.create_add_tx_order(&nonce, 5);
	let _worst_order = context.create_add_tx_order(&nonce, 1);
	// we must see first the most profitable order
	context.assert_pop_order(&best_order);
	// we simulate that best_order failed to execute so we don't call
	// update_onchain_nonces
	context.assert_pop_order(&middle_order);
	// we simulate that middle_order executed
	context.update_nonce(&nonce, 1);
	// we must see none and NOT _worst_order (invalid nonce)
	context.assert_pop_none();
}

#[test]
/// Tests 4 tx from the same account with different nonces.
fn test_block_orders_pending_orders() {
	let (nonce, mut context) = TestContext::new_1_account(0);
	let first_nonce_order = context.create_add_tx_order(&nonce, 3);
	let second_nonce_order_worst =
		context.create_add_tx_order(&nonce.clone().with_nonce(1), 5);
	let second_nonce_order_best =
		context.create_add_tx_order(&nonce.clone().with_nonce(1), 6);
	let _third_nonce_order =
		context.create_add_tx_order(&nonce.clone().with_nonce(2), 7);

	context.assert_pop_order(&first_nonce_order);
	// Until we update the execution we must see none
	context.assert_pop_none();
	// executed
	context.update_nonce(&nonce, 1);
	context.assert_pop_order(&second_nonce_order_best);
	// second_nonce_order_best failed so we don't update_nonce
	context.assert_pop_order(&second_nonce_order_worst);
	// No more orders for second nonce -> we must see none
	context.assert_pop_none();
	// sim that last tx increased nonce twice so we skipped third_nonce_order_best
	context.update_nonce(&nonce, 3);
	// _third_nonce_order_best was skipped -> none
	context.assert_pop_none();
}

#[test]
// Execute a bundle with an optional tx that fails for invalid nonce
fn test_block_orders_optional_nonce() {
	let (nonce_1, nonce_2, mut context) = TestContext::new_2_accounts(0, 0);
	let bundle_order =
		context.create_add_bundle_order_2_txs(&nonce_1, true, &nonce_2, false, 1);
	let tx_order = context.create_add_tx_order(&nonce_1, 2);

	// tx_order gives more profit
	context.assert_pop_order(&tx_order);
	// tx_order executed, now tx_order nonce_1 updates
	context.update_nonce(&nonce_1, 1);
	// Even with the first tx failing because of nonce_1 the bundle should be
	// valid
	context.assert_pop_order(&bundle_order);
	// No more orders
	context.assert_pop_none();
}
