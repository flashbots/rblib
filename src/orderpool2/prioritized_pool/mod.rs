/// Prioritized orderpool is used by block building thread to iterate
/// over all bundles using some ordering and include them while keeping
/// track of onchain nonces.
///
/// Usage:
/// 1. Create `PrioritizedOrderpool` and incrementally fill it with bundles
///    using `insert_order`
/// 2. Clone it before starting block building run
/// 3. Pop bundles using `pop_order` until it returns None and try to
///    include them
/// 4. Update onchain nonces after each successful commit using
///    `update_onchain_nonces`
use {
	crate::alloy::primitives::Address,
	priority_queue::PriorityQueue,
	std::collections::{HashMap, HashSet, hash_map::Entry},
};

use super::{AccountNonce, BundleNonce, OrderpoolNonceSource, OrderpoolOrder};

pub mod step;
#[cfg(test)]
mod tests;

pub trait PrioritizedOrderpoolPriority: Ord + Clone + Send + Sync {
	type Order;
	fn new(order: &Self::Order) -> Self;
}

#[derive(Debug, Clone)]
pub struct PrioritizedOrderpool<Priority, Order: OrderpoolOrder> {
	/// Ready (all nonce matching (or not matched but optional)) to execute
	/// orders sorted
	main_queue: PriorityQueue<Order::ID, Priority>,
	/// For each account we store all the orders from `main_queue` which contain
	/// a tx from this account. Since the orders belong to `main_queue` these
	/// are orders ready to execute. As soon as we execute an order from
	/// `main_queue` all orders for all the accounts the order used
	/// (`order.nonces()`) could get invalidated (if tx is not optional).
	main_queue_nonces: HashMap<Address, Vec<Order::ID>>,

	/// Up to date "onchain" nonces for the current block we are building.
	/// Special care must be taken to keep this in sync.
	onchain_nonces: HashMap<Address, u64>,

	/// Orders waiting for an account to reach a particular nonce.
	pending_orders: HashMap<AccountNonce, Vec<Order::ID>>,
	/// Id -> order for all orders we manage. Carefully maintained by
	/// remove/insert
	orders: HashMap<Order::ID, Order>,
}

impl<Priority: Ord, Order: OrderpoolOrder> Default
	for PrioritizedOrderpool<Priority, Order>
{
	fn default() -> Self {
		Self {
			main_queue: PriorityQueue::new(),
			main_queue_nonces: HashMap::default(),
			onchain_nonces: HashMap::default(),
			pending_orders: HashMap::default(),
			orders: HashMap::default(),
		}
	}
}

impl<Priority, Order> PrioritizedOrderpool<Priority, Order>
where
	Priority: PrioritizedOrderpoolPriority<Order = Order>,
	Order: OrderpoolOrder,
{
	/// Removes order from the pool
	/// # Panics
	/// Panics if implementation has a bug
	pub fn pop_order(&mut self) -> Option<Order> {
		let (id, _) = self.main_queue.pop()?;

		let order = self
			.remove_popped_order(&id)
			.expect("order from prio queue not found in block orders");
		Some(order)
	}

	/// Clean up after some order was removed from `main_queue`
	fn remove_popped_order(&mut self, id: &Order::ID) -> Option<Order> {
		let order = self.remove_from_orders(id)?;
		for BundleNonce { address, .. } in order.nonces() {
			match self.main_queue_nonces.entry(address) {
				Entry::Occupied(mut entry) => {
					entry.get_mut().retain(|id| *id != order.id());
				}
				Entry::Vacant(_) => {}
			}
		}
		Some(order)
	}

	/// Updates orderpool with changed nonces
	/// if order updates onchain nonce from n -> n + 2, we get n + 2 as an
	/// arguments here
	/// # Panics
	/// Panics if implementation has a bug
	pub fn update_onchain_nonces<NonceSource: OrderpoolNonceSource>(
		&mut self,
		new_nonces: &[AccountNonce],
		nonce_source: &NonceSource,
	) -> Result<(), NonceSource::NonceError> {
		let mut invalidated_orders: HashSet<Order::ID> = HashSet::default();
		for new_nonce in new_nonces {
			self
				.onchain_nonces
				.insert(new_nonce.account, new_nonce.nonce);

			if let Some(orders) = self.main_queue_nonces.remove(&new_nonce.account) {
				invalidated_orders.extend(orders.into_iter());
			}
		}

		for order_id in invalidated_orders {
			// check if order can still be valid because of optional nonces
			self.main_queue.remove(&order_id);
			let order = self
				.remove_popped_order(&order_id)
				.expect("order from prio queue not found in block orders");
			let mut valid = true;
			let mut valid_nonces = 0;
			for BundleNonce {
				nonce,
				address,
				optional,
			} in order.nonces()
			{
				let onchain_nonce = self.nonce(&address, nonce_source)?;
				if onchain_nonce > nonce && !optional {
					valid = false;
					break;
				} else if onchain_nonce == nonce {
					valid_nonces += 1;
				}
			}
			let retain_order = valid && valid_nonces > 0;
			if retain_order {
				self.insert_order(order, nonce_source)?;
			}
		}

		for new_nonce in new_nonces {
			if let Some(pending) = self.pending_orders.remove(new_nonce) {
				let orders = pending
					.iter()
					.filter_map(|id| self.remove_from_orders(id))
					.collect::<Vec<_>>();
				for order in orders {
					self.insert_order(order, nonce_source)?;
				}
			}
		}
		Ok(())
	}

	fn remove_from_orders(&mut self, id: &Order::ID) -> Option<Order> {
		self.orders.remove(id)
	}

	fn nonce<NonceSource: OrderpoolNonceSource>(
		&mut self,
		address: &Address,
		nonce_source: &NonceSource,
	) -> Result<u64, NonceSource::NonceError> {
		match self.onchain_nonces.entry(*address) {
			Entry::Occupied(entry) => Ok(*entry.get()),
			Entry::Vacant(entry) => {
				let nonce = nonce_source.nonce(address)?;
				entry.insert(nonce);
				Ok(nonce)
			}
		}
	}

	pub fn insert_order<NonceSource: OrderpoolNonceSource>(
		&mut self,
		order: Order,
		nonce_source: &NonceSource,
	) -> Result<(), NonceSource::NonceError> {
		if self.orders.contains_key(&order.id()) {
			return Ok(());
		}
		let mut pending_nonces = Vec::new();
		for BundleNonce {
			nonce,
			address,
			optional,
		} in order.nonces()
		{
			let onchain_nonce = self.nonce(&address, nonce_source)?;
			if onchain_nonce > nonce && !optional {
				// order can't be included because of nonce
				return Ok(());
			}
			if onchain_nonce < nonce && !optional {
				pending_nonces.push(AccountNonce {
					account: address,
					nonce,
				});
			}
		}
		if pending_nonces.is_empty() {
			self.main_queue.push(order.id(), Priority::new(&order));
			for nonce in order.nonces() {
				self
					.main_queue_nonces
					.entry(nonce.address)
					.or_default()
					.push(order.id());
			}
		} else {
			for pending_nonce in pending_nonces {
				let pending = self.pending_orders.entry(pending_nonce).or_default();
				if !pending.contains(&order.id()) {
					pending.push(order.id());
				}
			}
		}
		self.orders.insert(order.id(), order);
		Ok(())
	}

	pub fn remove_order(&mut self, id: &Order::ID) -> Option<Order> {
		// we don't remove from pending because pending will clean itself
		if self.main_queue.remove(id).is_some() {
			self.remove_popped_order(id);
		}
		self.remove_from_orders(id)
	}
}
