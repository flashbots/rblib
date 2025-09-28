use {
	super::{
		super::{OrderHash, PooledOrder},
		Group,
		GroupId,
		group::GroupEffect,
	},
	crate::{alloy, prelude::*},
	alloy::primitives::Address,
	rustc_hash::FxHashMap,
};

/// Container that manages all order groups and secondary indexes.
///
/// Responsibilities:
/// - Track groups by `GroupId`
/// - Maintain `by_signer` and `by_order` lookups for fast routing
/// - On insert: create, extend, or merge groups (merges prefer the largest
///   group)
/// - On discard: remove orders and handle group vanish/rename/split updates
pub struct Groups<P: Platform> {
	all: FxHashMap<GroupId, Group<P>>,
	by_signer: FxHashMap<Address, GroupId>,
	by_order: FxHashMap<OrderHash, GroupId>,
}

impl<P: Platform> Default for Groups<P> {
	fn default() -> Self {
		Self {
			all: FxHashMap::default(),
			by_signer: FxHashMap::default(),
			by_order: FxHashMap::default(),
		}
	}
}

impl<P: Platform> Groups<P> {
	/// Returns true if there are no groups.
	pub fn is_empty(&self) -> bool {
		self.all.is_empty()
	}

	/// Returns the total number of orders across all groups.
	pub fn len(&self) -> usize {
		self.by_order.len()
	}

	/// Returns a reference to the group by id, if present.
	pub fn get(&self, id: &GroupId) -> Option<&Group<P>> {
		self.all.get(id)
	}

	/// Returns true if an order with the given hash exists.
	pub fn contains_order(&self, hash: &OrderHash) -> bool {
		self.by_order.contains_key(hash)
	}

	/// Insert an order into the groups structure.
	///
	/// Behavior:
	/// - If the order shares no signer with any existing group, create a new
	///   group.
	/// - If it overlaps with a single group, insert into that group (the
	///   `GroupId` may update if the anchor shrinks).
	/// - If it bridges multiple groups, they are merged into the largest group
	///   and then the order is inserted.
	///
	/// Returns the `GroupId` that now owns the order.
	pub fn insert(&mut self, order: PooledOrder<P>) -> GroupId {
		let order_hash = order.hash();

		// Fast path: already present
		if let Some(gid) = self.by_order.get(&order_hash) {
			return *gid;
		}

		let signers = order.signers().clone();
		let matching_groups = self.overlapping_signers(signers.iter());

		match matching_groups.len() {
			0 => {
				// No overlap → new group
				let group = Group::new(order);
				let gid = group.id();
				self.index_new_group(gid, &group);
				self.by_order.insert(order_hash, gid);
				self.all.insert(gid, group);
				gid
			}
			1 => {
				// Extend existing group
				let current_id = matching_groups[0];
				let group = self.all.get_mut(&current_id).expect("group must exist");
				let Ok(new_id) = group.insert(order) else {
					// already present or disjoint (shouldn't be disjoint due to
					// mapping)
					return current_id;
				};

				if new_id != current_id {
					// Anchor changed → reindex
					let group = self.all.remove(&current_id).expect("present");
					self.reindex_group_replace(current_id, new_id, group);
				}
				// Ensure any new signers introduced by the order are mapped
				for a in signers {
					self.by_signer.insert(a, new_id);
				}
				self.by_order.insert(order_hash, new_id);
				new_id
			}
			_ => {
				// Bridge multiple groups: merge into the largest group to minimize
				// moves
				let mut ids = matching_groups;
				ids.sort_by_key(|gid| self.all.get(gid).map_or(0, |g| g.len()));
				let primary_id = ids.pop().expect("non-empty");
				let mut primary = self.all.remove(&primary_id).expect("present");
				// absorb all others into primary without cloning orders
				for gid in ids.into_iter().rev() {
					if let Some(other) = self.all.remove(&gid) {
						// cleanup indices for other
						self.unindex_group_id(&gid);
						let _ = primary.absorb(other);
					}
				}

				// Insert the new order into the merged primary
				let prev_id = primary.id();
				let res_id = match primary.insert(order) {
					Ok(id) => id,
					Err(_) => prev_id,
				};
				let new_id = res_id;
				// Reindex signers and orders for the merged group under new_id
				self.index_group_full(new_id, &primary);
				self.by_order.insert(order_hash, new_id);
				self.all.insert(new_id, primary);
				new_id
			}
		}
	}

	/// Discard (remove) an order by its hash.
	///
	/// Behavior:
	/// - Delegates to the underlying group's removal logic; does NOT advance
	///   nonce frontiers (mutually exclusive alternatives remain eligible).
	/// - Applies structural outcomes: vanish, rename (anchor change), or split
	///   into children, and updates `by_signer`/`by_order` indexes accordingly.
	/// - Returns the exact removed order together with a `RemoveResult` that
	///   summarizes the structural change.
	///
	/// Returns `Some((removed_order, outcome))` when the order existed, or
	/// `None` when no such order is indexed.
	pub fn discard(
		&mut self,
		hash: OrderHash,
	) -> Option<(PooledOrder<P>, RemoveResult)> {
		let current_id = *self.by_order.get(&hash)?;
		let mut group = self.all.remove(&current_id)?;

		match group.remove(&hash) {
			Some((removed, GroupEffect::Vanished { .. })) => {
				// Remove all indexes for this group
				self.unindex_group_id(&current_id);
				// Remove order mapping for removed
				self.by_order.remove(&hash);
				Some((removed, RemoveResult::Vanished))
			}
			Some((
				removed,
				GroupEffect::StillConnected {
					old_id,
					new_id,
					removed_signers,
				},
			)) => {
				// Remove the discarded order mapping
				self.by_order.remove(&hash);
				// Remove any signers that dropped out entirely
				for a in removed_signers {
					self.by_signer.remove(&a);
				}
				// Ensure all current orders are correctly mapped and stale ones purged
				self.reindex_group_orders_replace(old_id, new_id, &group);
				// Ensure by_signer for remaining signers point to new_id
				for a in group.signers() {
					self.by_signer.insert(a, new_id);
				}
				// Put back the updated group under its (possibly new) id
				self.all.insert(new_id, group);
				Some((removed, RemoveResult::Updated(new_id)))
			}
			Some((
				removed,
				GroupEffect::Split {
					old_id, children, ..
				},
			)) => {
				// Remove old indices and insert children
				self.unindex_group_id(&old_id);
				self.by_order.remove(&hash);
				let mut new_ids = Vec::with_capacity(children.len());
				for child in children {
					let gid = child.id();
					self.index_group_full(gid, &child);
					self.all.insert(gid, child);
					new_ids.push(gid);
				}
				Some((removed, RemoveResult::Split(new_ids)))
			}
			None => None,
		}
	}

	/// Consume an order by its hash: advances nonce frontiers, prunes mutually
	/// exclusive orders, and applies structural changes to groups and indexes.
	///
	/// Returns `Some(ConsumeResult)` when the order existed, or `None` otherwise.
	pub fn consume(&mut self, hash: OrderHash) -> Option<ConsumeResult<P>> {
		let current_id = *self.by_order.get(&hash)?;
		let mut group = self.all.remove(&current_id)?;

		let outcome = group.consume(&hash)?;

		// Always remove consumed and pruned orders from by_order before reindexing
		self.by_order.remove(&hash);
		for po in &outcome.pruned {
			self.by_order.remove(&po.hash());
		}

		let remove_result = match outcome.group {
			GroupEffect::Vanished { .. } => {
				// Purge all indices for the vanished group
				self.unindex_group_id(&current_id);
				RemoveResult::Vanished
			}
			GroupEffect::StillConnected {
				old_id,
				new_id,
				removed_signers,
			} => {
				// Remove signers that dropped out entirely
				for a in removed_signers {
					self.by_signer.remove(&a);
				}
				// Replace by_order entries for the group's remaining orders
				self.reindex_group_orders_replace(old_id, new_id, &group);
				// Ensure by_signer entries for current signers point to new_id
				for a in group.signers() {
					self.by_signer.insert(a, new_id);
				}
				// Put back the updated group
				self.all.insert(new_id, group);
				RemoveResult::Updated(new_id)
			}
			GroupEffect::Split {
				old_id, children, ..
			} => {
				// Purge indices for the old group and insert children
				self.unindex_group_id(&old_id);
				let mut new_ids = Vec::with_capacity(children.len());
				for child in children {
					let gid = child.id();
					self.index_group_full(gid, &child);
					self.all.insert(gid, child);
					new_ids.push(gid);
				}
				return Some(ConsumeResult {
					consumed: outcome.consumed,
					pruned: outcome.pruned,
					outcome: RemoveResult::Split(new_ids),
				});
			}
		};

		Some(ConsumeResult {
			consumed: outcome.consumed,
			pruned: outcome.pruned,
			outcome: remove_result,
		})
	}
}

/// Internal implementation details
impl<P: Platform> Groups<P> {
	fn overlapping_signers<'a>(
		&mut self,
		signers: impl Iterator<Item = &'a Address>,
	) -> Vec<GroupId> {
		let mut candidate_ids: Vec<GroupId> = Vec::new();

		for a in signers {
			if let Some(gid) = self.by_signer.get(a) {
				if !candidate_ids.contains(gid) {
					candidate_ids.push(*gid);
				}
			}
		}

		candidate_ids
	}

	fn index_new_group(&mut self, gid: GroupId, group: &Group<P>) {
		for a in group.signers() {
			self.by_signer.insert(a, gid);
		}
	}

	fn index_group_full(&mut self, gid: GroupId, group: &Group<P>) {
		// index signers (only modify entries that differ)
		for a in group.signers() {
			match self.by_signer.get_mut(&a) {
				Some(slot) if *slot != gid => *slot = gid,
				None => {
					self.by_signer.insert(a, gid);
				}
				_ => {}
			}
		}
		// index orders
		for h in group.order_hashes() {
			self.by_order.insert(h, gid);
		}
	}

	fn index_group_partial(&mut self, gid: GroupId, group: &Group<P>) {
		for a in group.signers() {
			self.by_signer.insert(a, gid);
		}
	}

	fn unindex_group_id(&mut self, gid: &GroupId) {
		// purge by_signer entries matching gid
		let to_purge: Vec<Address> = self
			.by_signer
			.iter()
			.filter_map(|(a, g)| if *g == *gid { Some(*a) } else { None })
			.collect();
		for a in to_purge {
			self.by_signer.remove(&a);
		}
		// purge by_order entries matching gid
		let to_purge_orders: Vec<OrderHash> = self
			.by_order
			.iter()
			.filter_map(|(h, g)| if *g == *gid { Some(*h) } else { None })
			.collect();
		for h in to_purge_orders {
			self.by_order.remove(&h);
		}
	}

	fn reindex_group_replace(
		&mut self,
		_old_id: GroupId,
		new_id: GroupId,
		group: Group<P>,
	) {
		// Update by_signer: ensure all current signers point to new_id (only modify
		// differing ones)
		for a in group.signers() {
			match self.by_signer.get_mut(&a) {
				Some(slot) if *slot != new_id => *slot = new_id,
				None => {
					self.by_signer.insert(a, new_id);
				}
				_ => {}
			}
		}

		// Update by_order for all orders currently in the group
		for h in group.order_hashes() {
			self.by_order.insert(h, new_id);
		}

		// Put back the group under its new id
		self.all.insert(new_id, group);
	}

	/// Replace the by_order mappings for a group's orders after a mutation,
	/// ensuring stale entries tied to `old_id` are purged and current orders
	/// point to `new_id`.
	fn reindex_group_orders_replace(
		&mut self,
		old_id: GroupId,
		new_id: GroupId,
		group: &Group<P>,
	) {
		// Purge all by_order entries that were associated with old_id
		let to_purge_orders: Vec<OrderHash> = self
			.by_order
			.iter()
			.filter_map(|(h, g)| if *g == old_id { Some(*h) } else { None })
			.collect();
		for h in to_purge_orders {
			self.by_order.remove(&h);
		}
		// Reinsert current group's orders pointing to new_id
		for h in group.order_hashes() {
			self.by_order.insert(h, new_id);
		}
	}
}

/// Result of removing an order from the `Groups` collection.
#[derive(Debug, Clone)]
pub enum RemoveResult {
	/// Updated a single group (may have a new `GroupId`).
	Updated(GroupId),
	/// The group vanished entirely.
	Vanished,
	/// The group split into multiple child groups. Contains their ids.
	Split(Vec<GroupId>),
}

/// Result of consuming an order at the collection level.
#[derive(Debug, Clone)]
pub struct ConsumeResult<P: Platform> {
	/// The order that was consumed (treated as included on-chain).
	pub consumed: PooledOrder<P>,
	/// Conflicting orders that were pruned as a consequence of consumption.
	pub pruned: Vec<PooledOrder<P>>,
	/// Structural change to the owning group(s) after applying the consume.
	pub outcome: RemoveResult,
}

#[cfg(test)]
mod tests {
	// Bring in the collection under test and supporting types/utilities.
	use {
		super::*,
		crate::{
			platform::FlashbotsBundle,
			pool::{Order, PooledOrder, tests::make_tx},
			test_utils::{FundedAccounts, TestablePlatform, rblib_test},
		},
		itertools::Itertools,
	};

	#[rblib_test(Ethereum, Optimism)]
	fn insert_new_and_contains<P: TestablePlatform>() {
		let mut groups: Groups<P> = Groups::default();
		let signers = (0..1)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let o: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[0], 0)).into();
		let gid = groups.insert(o.clone());
		assert!(groups.contains_order(&o.hash()));
		assert!(groups.get(&gid).is_some());
		assert_eq!(groups.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_merges_into_largest<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2 < s3 (lexicographic by address)
		let mut groups: Groups<P> = Groups::default();
		let signers = (0..4)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let o01: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[0], 0))
				.with_transaction(make_tx::<P>(&signers[1], 0)),
		)
		.into(); // group A size 1
		let o1: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[1], 1)).into(); // extend A -> size 2
		let o23: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[2], 0))
				.with_transaction(make_tx::<P>(&signers[3], 0)),
		)
		.into(); // group B size 1 (disjoint)

		let gid_a = groups.insert(o01.clone());
		let _ = groups.insert(o1.clone());
		let gid_b = groups.insert(o23.clone());
		assert_ne!(gid_a, gid_b);

		// Now insert an order bridging A and B: bundle with {s1, s2}
		let bridge: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[1], 3))
				.with_transaction(make_tx::<P>(&signers[2], 1)),
		)
		.into();
		let gid = groups.insert(bridge.clone());

		// It should merge both groups into a single one. The resulting id may
		// change (epoch bump, possibly new anchor), but B must be gone and
		// orders from both A and B must be in the resulting group.
		assert!(groups.get(&gid_b).is_none());
		let g = groups.get(&gid).expect("merged group present");
		assert!(g.contains_order(&o01.hash()));
		assert!(g.contains_order(&o23.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_absent_returns_none<P: TestablePlatform>() {
		let mut groups: Groups<P> = Groups::default();
		assert!(groups.discard(OrderHash::from([0u8; 32])).is_none());
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_vanishes_last_order<P: TestablePlatform>() {
		let mut groups: Groups<P> = Groups::default();
		let signers = (0..1)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let o: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[0], 0)).into();
		let gid = groups.insert(o.clone());
		let (removed, res) = groups.discard(o.hash()).expect("present");
		assert_eq!(removed.hash(), o.hash());
		match res {
			RemoveResult::Vanished => {}
			_ => panic!("expected Vanished"),
		}
		assert!(groups.get(&gid).is_none());
		assert_eq!(groups.len(), 0);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_updated_same_group_or_new_id<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let mut groups: Groups<P> = Groups::default();
		// Start with a bundle {s0,s1} and then add {s1} so removing {s1} keeps
		// connected
		let signers = (0..2)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let o01: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[0], 0))
				.with_transaction(make_tx::<P>(&signers[1], 0)),
		)
		.into();
		let gid0 = groups.insert(o01.clone());
		let o1: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[1], 1)).into();
		let _ = groups.insert(o1.clone());
		let (_removed, res) = groups.discard(o1.hash()).expect("present");
		match res {
			RemoveResult::Updated(id) => assert_eq!(id, gid0),
			_ => panic!("expected Updated"),
		}
		assert!(groups.contains_order(&o01.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_split_creates_children<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2 < s3 (lexicographic by address)
		let mut groups: Groups<P> = Groups::default();
		let signers = (0..4)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let o01: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[0], 0))
				.with_transaction(make_tx::<P>(&signers[1], 0)),
		)
		.into();
		let o12: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[1], 1))
				.with_transaction(make_tx::<P>(&signers[2], 0)),
		)
		.into(); // bridge order
		let o23: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[2], 1))
				.with_transaction(make_tx::<P>(&signers[3], 0)),
		)
		.into();
		let gid0 = groups.insert(o01.clone());
		let _ = groups.insert(o12.clone());
		let _ = groups.insert(o23.clone());
		// removing the bridge splits into two child groups
		let (removed, res) = groups.discard(o12.hash()).expect("present");
		let RemoveResult::Split(ids) = res else {
			panic!("expected Split");
		};

		assert!(ids.len() >= 2);
		// Original group id should no longer exist
		assert!(groups.get(&gid0).is_none());
		assert_eq!(removed.hash(), o12.hash());
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_multi_merge_chain_unifies_all<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// Build three disjoint groups: A:{s0,s1}+{s1}, B:{s2,s3}, C:{s4,s5}
		// (lexicographic)
		let mut groups: Groups<P> = Groups::default();
		let signers = (0..6)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let a01: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[0], 0))
				.with_transaction(make_tx::<P>(&signers[1], 0)),
		)
		.into();
		let a1: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[1], 1)).into(); // extend A
		let b23: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[2], 0))
				.with_transaction(make_tx::<P>(&signers[3], 0)),
		)
		.into();
		let c45: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[4], 0))
				.with_transaction(make_tx::<P>(&signers[5], 0)),
		)
		.into();

		let gid_a = groups.insert(a01.clone());
		let _ = groups.insert(a1.clone());
		let gid_b = groups.insert(b23.clone());
		let gid_c = groups.insert(c45.clone());
		assert_ne!(gid_a, gid_b);
		assert_ne!(gid_b, gid_c);

		// Merge B and C via a bridge between {s3,s4}
		let bc_bridge: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[3], 1))
				.with_transaction(make_tx::<P>(&signers[4], 1)),
		)
		.into();
		let gid_bc = groups.insert(bc_bridge.clone());
		// C should be gone after the merge; orders from both B and C must be
		// present
		assert!(groups.get(&gid_c).is_none());
		let g_bc = groups.get(&gid_bc).expect("merged BC present");
		assert!(g_bc.contains_order(&b23.hash()));
		assert!(g_bc.contains_order(&c45.hash()));

		// Now merge (B∪C) with A via a bridge between {s1,s4}
		let abc_bridge: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[1], 2))
				.with_transaction(make_tx::<P>(&signers[4], 2)),
		)
		.into();
		let gid_abc = groups.insert(abc_bridge.clone());
		// Both old B and C ids must no longer exist; resulting group should contain
		// all orders
		assert!(groups.get(&gid_b).is_none());
		assert!(groups.get(&gid_c).is_none());
		let g_abc = groups.get(&gid_abc).expect("final merged group present");
		for h in [
			a01.hash(),
			a1.hash(),
			b23.hash(),
			c45.hash(),
			bc_bridge.hash(),
			abc_bridge.hash(),
		] {
			assert!(g_abc.contains_order(&h));
		}
		// Sanity: total order count across groups equals inserted orders
		assert_eq!(groups.len(), 6);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_anchor_change_reindexes<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// Choose three funded accounts by sorted address and create a chain
		// {anchor,mid} and {mid,max}; removing the first drops the anchor
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let mut groups: Groups<P> = Groups::default();
		let o_anchor_mid: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[0], 0))
				.with_transaction(make_tx::<P>(&signers[1], 0)),
		)
		.into();
		let o_mid_max: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[1], 1))
				.with_transaction(make_tx::<P>(&signers[2], 0)),
		)
		.into();
		let old_id = groups.insert(o_anchor_mid.clone());
		let _ = groups.insert(o_mid_max.clone());

		let (_removed, res) = groups.discard(o_anchor_mid.hash()).expect("present");
		let RemoveResult::Updated(new_id) = res else {
			panic!("expected Updated with anchor change");
		};

		// Old id should be gone after reindex, remaining order still present
		assert!(groups.get(&old_id).is_none());
		let g = groups.get(&new_id).expect("reindexed group present");
		assert!(g.contains_order(&o_mid_max.hash()));
		// The new anchor should be the middle address
		assert_eq!(new_id.anchor(), signers[1].address());
		// Total orders now 1
		assert_eq!(groups.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn split_updates_by_signer_index_for_followup_insert<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// Construct 0-1-2-3 chain and split by removing {1,2}, then insert new {0}
		let mut groups: Groups<P> = Groups::default();
		let signers = (0..4)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let o01: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[0], 0))
				.with_transaction(make_tx::<P>(&signers[1], 0)),
		)
		.into();
		let o12: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[1], 1))
				.with_transaction(make_tx::<P>(&signers[2], 0)),
		)
		.into();
		let o23: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(make_tx::<P>(&signers[2], 1))
				.with_transaction(make_tx::<P>(&signers[3], 0)),
		)
		.into();
		let _ = groups.insert(o01.clone());
		let _ = groups.insert(o12.clone());
		let _ = groups.insert(o23.clone());

		let (removed, res) = groups.discard(o12.hash()).expect("present");
		assert_eq!(removed.hash(), o12.hash());
		let RemoveResult::Split(child_ids) = &res else {
			panic!("expected Split");
		};
		assert!(!child_ids.is_empty());

		// Find the child that still contains o01
		let mut left_gid = None;
		for gid in child_ids.iter().copied() {
			if let Some(g) = groups.get(&gid) {
				if g.contains_order(&o01.hash()) {
					left_gid = Some(gid);
					break;
				}
			}
		}
		let left_gid = left_gid.expect("child containing o01 present");

		// Insert a new single-signer order for signer 0; it must go to the left
		// child
		let o0_new: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[0], 99)).into();
		let new_gid = groups.insert(o0_new.clone());
		// Either anchor changed (id replaced) or stayed the same; handle both
		let grp = if let Some(g) = groups.get(&left_gid) {
			g
		} else {
			groups.get(&new_gid).expect("group present after insert")
		};
		assert!(grp.contains_order(&o01.hash()));
		assert!(grp.contains_order(&o0_new.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_absent_returns_none<P: TestablePlatform>() {
		let mut groups: Groups<P> = Groups::default();
		assert!(groups.consume(OrderHash::from([0u8; 32])).is_none());
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_prunes_conflicts_and_updates_indexes<P: TestablePlatform>() {
		// Two single-signer txs with the same signer and overlapping nonce ->
		// conflicts
		let signers = (0..1)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let mut groups: Groups<P> = Groups::default();
		let o0a: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[0], 0)).into();
		let o0b: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[0], 0)).into();
		let _gid = groups.insert(o0a.clone());
		let _gid2 = groups.insert(o0b.clone());

		let res = groups.consume(o0a.hash()).expect("present");
		assert_eq!(res.consumed.hash(), o0a.hash());
		// The other order should be pruned
		assert_eq!(res.pruned.len(), 1);
		assert_eq!(res.pruned[0].hash(), o0b.hash());
		match res.outcome {
			RemoveResult::Vanished => {}
			_ => panic!("expected Vanished after pruning last order"),
		}
		// Indices should be empty
		assert!(groups.is_empty());
		assert_eq!(groups.len(), 0);
	}
}
