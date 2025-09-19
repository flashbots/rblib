//! This module implements nonce-signer dependency trakcing for orders in the
//! pool.

use {
	super::*,
	crate::{alloy, pool::nonce::NonceState},
	alloy::primitives::Address,
	derive_more::Deref,
	group::{GroupId, OrderGroup},
	std::collections::HashSet,
};

mod group;

/// This structure holds all orders in the pool, linked by their nonce
/// dependencies. It is responsible for identifying inter-order dependencies and
/// managing their proposal order.
///
/// Conceptually it is a directed graph of inter-order dependencies, where each
/// connected component forms a distinct `OrderGroup`. Independent orders are
/// their own groups of one.
///
/// This is an immutable data structure, any modification returns a new version
/// of the graph. This design allows this graph to be cheaply snapshotted and
/// given to new `OrdersStream` instances. Under the covers this is optimized
/// for performance by using structural sharing.
#[derive(Default)]
pub struct OrderGraph<P: Platform> {
	/// All order groups in this graph, indexed by their unique Identifier.
	/// The identifiers have no meaning outside of this structure and are
	/// randomly generated when a new group is created.
	groups: im::HashMap<GroupId, OrderGroup<P>>,

	/// Mapping from an order hash to the group it belongs to.
	orders: im::HashMap<OrderHash, GroupId>,

	/// Mapping from a signer address to its membership in an `OrderGroup`.
	///
	/// A signer can be at most in one `OrderGroup` at a time. If a new order
	/// arrives that shares signers with multiple groups, those groups are
	/// merged.
	///
	/// If an order leaves the pool and partitions an order group into multiple
	/// disconnected components, the components are split into separate groups.
	signers: im::HashMap<Address, GroupId>,

	/// The latest known nonce for each address
	nonces: NonceState,
}

impl<P: Platform> Clone for OrderGraph<P> {
	fn clone(&self) -> Self {
		Self {
			groups: self.groups.clone(),
			orders: self.orders.clone(),
			signers: self.signers.clone(),
			nonces: self.nonces.clone(),
		}
	}
}

/// Public API - mutations
impl<P: Platform> OrderGraph<P> {
	pub fn update_nonces(&mut self, nonces: NonceState) {
		let affected_groups: HashSet<_> = nonces
			.iter()
			.filter_map(|(addr, _)| self.signers.get(addr).copied())
			.collect();

		for group_id in affected_groups {
			let mut group = self.groups.remove(&group_id).expect("group exists; qed");
			let relevant_nonces = nonces.iter().filter_map(|(addr, nonce)| {
				group.signers().contains(addr).then(|| (*addr, *nonce))
			});

			let update_result = group.update_nonces(relevant_nonces.collect());
			for signer in update_result.dropped_signers {
				self.signers.remove(&signer);
			}
			for order in update_result.dropped_orders {
				self.orders.remove(&order.hash());
			}
			for group in update_result.output_groups {
				self.insert_group(group);
			}
		}

		self.nonces.update(nonces);
	}

	/// Inserts a new order into the graph, creating or merging order groups as
	/// needed.
	pub fn insert(&mut self, order: PooledOrder<P>) {
		if self.orders.contains_key(&order.hash()) {
			// order already present, noop
			return;
		}

		if self.nonces.is_eligible(&order)
			== Some(Eligibility::PermanentlyIneligible)
		{
			// order is permanently ineligible, noop
			return;
		}

		// First collect all potential order groups that share signers with this
		// order. If any found, they will all be merged into one group that will
		// contain the new order as well. Then create a new order group by merging
		// the new order with all matching groups.
		let group = self
			.extract_matching(&order)
			.into_iter()
			.try_fold(OrderGroup::new(order), |g, other| g.merge(other))
			.expect("groups must share at least one signer with the new order; qed");

		// Finally insert the new supergroup into the graph, updating all indices.
		self.insert_group(group);
	}

	/// Discards an order from the graph. If the order is not present, this is a
	/// no-op. Otherwise the order is removed from this graph and all dependent
	/// orders are also affected.
	pub fn discard(&mut self, order_hash: OrderHash) {
		let Some(group_id) = self.orders.remove(&order_hash) else {
			// order not present, noop
			return;
		};

		let Some(group) = self.groups.remove(&group_id) else {
			// group not present, noop
			return;
		};

		let result = group.discard(order_hash);

		for signer in result.dropped_signers {
			self.signers.remove(&signer);
		}

		for group in result.output_groups {
			self.insert_group(group);
		}

		for order in result.dropped_orders {
			self.orders.remove(&order.hash());
		}
	}

	/// Returns the next best order from the graph, removing it from the graph and
	/// committing to it. This will update the nonces of all signers involved
	/// and may cause other orders to become ineligible and be dropped from the
	/// graph as well.
	pub fn pop(&mut self) -> Option<PooledOrder<P>> {
		let group_id = GroupId::random(); // todo!("select best group id");
		let group = self.remove_group(&group_id)?;
		let pop_result = group.pop();

		if let Some(order) = pop_result.order {
			for signer in pop_result.removal.dropped_signers {
				self.signers.remove(&signer);
			}

			for order in pop_result.removal.dropped_orders {
				self.orders.remove(&order.hash());
			}

			for group in pop_result.removal.output_groups {
				self.insert_group(group);
			}

			self.update_nonces(pop_result.nonces);

			Some(order)
		} else {
			None
		}
	}
}

/// Internal API
impl<P: Platform> OrderGraph<P> {
	/// Removes an order group from the graph, cleaning up all associated indices.
	fn remove_group(&mut self, id: &GroupId) -> Option<OrderGroup<P>> {
		let group = self.groups.remove(id)?;
		for signer in group.signers() {
			assert_eq!(
				self.signers.remove(&signer),
				Some(*id),
				"signer must map to this group; qed"
			);
		}
		for order in group.orders() {
			assert_eq!(
				self.orders.remove(&order.hash()),
				Some(*id),
				"order must map to this group; qed"
			);
		}
		Some(group)
	}

	/// Inserts a new order group into the graph, updating all associated indices.
	/// The group ID is randomly generated and returned.
	fn insert_group(&mut self, group: OrderGroup<P>) -> GroupId {
		let group_id = loop {
			let id = GroupId::random();
			if !self.groups.contains_key(&id) {
				break id;
			}
		};

		for signer in group.signers() {
			assert_eq!(
				self.signers.insert(signer, group_id),
				None,
				"signer must be unique to this group"
			);
		}

		for order in group.orders() {
			assert_eq!(
				self.orders.insert(order.hash(), group_id),
				None,
				"order must be unique to this group"
			);
		}

		assert!(
			self.groups.insert(group_id, group).is_none(),
			"group id must be unique; qed"
		);

		group_id
	}

	/// Returns an iterators over all order groups that share at least one
	/// signer with the given order.
	fn matching_groups(&self, order: &Order<P>) -> impl Iterator<Item = GroupId> {
		order
			.signers()
			.into_iter()
			.filter_map(|signer| self.signers.get(&signer).copied())
	}

	/// Removes all order groups that share at least one signer with the given
	/// order and returns them.
	fn extract_matching(&mut self, order: &Order<P>) -> Vec<OrderGroup<P>> {
		let matching_groups: Vec<_> = self.matching_groups(order).collect();
		matching_groups
			.into_iter()
			.map(|id| self.remove_group(&id).expect("group exists; qed"))
			.collect()
	}
}
