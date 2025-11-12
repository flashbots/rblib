use {
	super::{AccountNonce, OrderpoolNonceSource, OrderpoolOrder},
	crate::alloy::primitives::Address,
	std::{
		cmp::{Ordering, min},
		collections::{HashMap, HashSet, hash_map::Entry},
		hash::Hash,
	},
	tracing::error,
};

pub type SimulationId = u64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimulationRequest<Order> {
	pub id: SimulationId,
	pub order: Order,
	pub parents: Vec<Order>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatedResult<Order, SimResult> {
	pub id: SimulationId,
	pub simulated_order: SimResult,
	pub order: Order,
	pub previous_orders: Vec<Order>,
	pub nonces_after: Vec<AccountNonce>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingOrder<Order> {
	order: Order,
	unsatisfied_nonces: usize,
}

#[derive(Debug)]
enum OrderNonceState<Order> {
	Invalid,
	PendingNonces(Vec<AccountNonce>),
	Ready(Vec<Order>),
}

pub trait SimTreeResult: Clone {
	type ScoreType: Ord;
	fn score(&self) -> Self::ScoreType;
}

#[derive(Debug)]
pub struct SimTree<NonceSource, Order: OrderpoolOrder, Result> {
	// fields for nonce management
	nonces: NonceSource,

	sims: HashMap<SimulationId, SimulatedResult<Order, Result>>,
	sims_that_update_one_nonce: HashMap<AccountNonce, SimulationId>,

	pending_orders: HashMap<Order::ID, PendingOrder<Order>>,
	pending_nonces: HashMap<AccountNonce, Vec<Order::ID>>,

	ready_orders: Vec<SimulationRequest<Order>>,
}

impl<NonceSource, Order, SimResult> SimTree<NonceSource, Order, SimResult>
where
	Order: OrderpoolOrder + Clone,
	NonceSource: OrderpoolNonceSource,
	SimResult: SimTreeResult,
{
	pub fn new(nonces: NonceSource) -> Self {
		Self {
			nonces,
			sims: HashMap::default(),
			sims_that_update_one_nonce: HashMap::default(),
			pending_orders: HashMap::default(),
			pending_nonces: HashMap::default(),
			ready_orders: Vec::default(),
		}
	}

	pub fn push_order(
		&mut self,
		order: Order,
	) -> Result<(), NonceSource::NonceError> {
		if self.pending_orders.contains_key(&order.id()) {
			return Ok(());
		}

		let order_nonce_state = self.get_order_nonce_state(&order)?;

		match order_nonce_state {
			OrderNonceState::Invalid => {
				return Ok(());
			}
			OrderNonceState::PendingNonces(pending_nonces) => {
				let unsatisfied_nonces = pending_nonces.len();
				for nonce in pending_nonces {
					self
						.pending_nonces
						.entry(nonce)
						.or_default()
						.push(order.id());
				}
				self.pending_orders.insert(order.id(), PendingOrder {
					order,
					unsatisfied_nonces,
				});
			}
			OrderNonceState::Ready(parents) => {
				self.ready_orders.push(SimulationRequest {
					id: rand::random(),
					order,
					parents,
				});
			}
		}
		Ok(())
	}

	fn get_order_nonce_state(
		&mut self,
		order: &Order,
	) -> Result<OrderNonceState<Order>, NonceSource::NonceError> {
		let mut onchain_nonces_incremented: HashSet<Address> = HashSet::default();
		let mut pending_nonces = Vec::new();
		let mut parent_orders = Vec::new();

		for nonce in order.nonces() {
			let onchain_nonce = self.nonces.nonce(&nonce.address)?;

			match onchain_nonce.cmp(&nonce.nonce) {
				Ordering::Equal => {
					// nonce, valid
					onchain_nonces_incremented.insert(nonce.address);
				}
				Ordering::Greater => {
					// nonce invalid, maybe its optional
					if !nonce.optional {
						// this order will never be valid
						return Ok(OrderNonceState::Invalid);
					}
				}
				Ordering::Less => {
					if onchain_nonces_incremented.contains(&nonce.address) {
						// we already considered this account nonce
						continue;
					}
					// mark this nonce as considered
					onchain_nonces_incremented.insert(nonce.address);

					let nonce_key = AccountNonce {
						account: nonce.address,
						nonce: nonce.nonce,
					};

					if let Some(sim_id) = self.sims_that_update_one_nonce.get(&nonce_key)
					{
						// we have something that fills this nonce
						let sim = self.sims.get(sim_id).expect("we never delete sims");
						parent_orders.extend_from_slice(&sim.previous_orders);
						parent_orders.push(sim.order.clone());
						continue;
					}

					pending_nonces.push(nonce_key);
				}
			}
		}

		if pending_nonces.is_empty() {
			Ok(OrderNonceState::Ready(parent_orders))
		} else {
			Ok(OrderNonceState::PendingNonces(pending_nonces))
		}
	}

	pub fn push_orders(
		&mut self,
		orders: Vec<Order>,
	) -> Result<(), NonceSource::NonceError> {
		for order in orders {
			self.push_order(order)?;
		}
		Ok(())
	}

	pub fn pop_simulation_tasks(
		&mut self,
		limit: usize,
	) -> Vec<SimulationRequest<Order>> {
		let limit = min(limit, self.ready_orders.len());
		self.ready_orders.drain(..limit).collect()
	}

	// we don't really need state here because nonces are cached but its smaller
	// if we reuse pending state fn
	fn process_simulation_task_result(
		&mut self,
		result: &SimulatedResult<Order, SimResult>,
	) -> Result<(), NonceSource::NonceError> {
		self.sims.insert(result.id, result.clone());
		let mut orders_ready = Vec::new();
		if result.nonces_after.len() == 1 {
			let updated_nonce = result.nonces_after.first().unwrap().clone();

			match self.sims_that_update_one_nonce.entry(updated_nonce.clone()) {
				Entry::Occupied(mut entry) => {
					let current_score = {
						let sim_id = entry.get_mut();
						self
							.sims
							.get(sim_id)
							.expect("we never delete sims")
							.simulated_order
							.score()
					};
					if result.simulated_order.score() > current_score {
						entry.insert(result.id);
					}
				}
				Entry::Vacant(entry) => {
					entry.insert(result.id);

					if let Some(pending_orders) =
						self.pending_nonces.remove(&updated_nonce)
					{
						for order in pending_orders {
							match self.pending_orders.entry(order) {
								Entry::Occupied(mut entry) => {
									let pending_order = entry.get_mut();
									pending_order.unsatisfied_nonces -= 1;
									if pending_order.unsatisfied_nonces == 0 {
										orders_ready.push(entry.remove().order);
									}
								}
								Entry::Vacant(_) => {
									error!("SimTree bug order not found");
								}
							}
						}
					}
				}
			}
		}

		for ready_order in orders_ready {
			let pending_state = self.get_order_nonce_state(&ready_order)?;
			match pending_state {
				OrderNonceState::Ready(parents) => {
					self.ready_orders.push(SimulationRequest {
						id: rand::random(),
						order: ready_order,
						parents,
					});
				}
				OrderNonceState::Invalid => {
					error!("SimTree bug order became invalid");
				}
				OrderNonceState::PendingNonces(_) => {
					error!("SimTree bug order became pending again");
				}
			}
		}
		Ok(())
	}

	pub fn submit_simulation_tasks_results(
		&mut self,
		results: &[SimulatedResult<Order, SimResult>],
	) -> Result<(), NonceSource::NonceError> {
		for result in results {
			self.process_simulation_task_result(result)?;
		}
		Ok(())
	}

	pub fn is_ready(&self) -> bool {
		!self.ready_orders.is_empty()
	}
}
