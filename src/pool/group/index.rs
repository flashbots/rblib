use {super::*, rustc_hash::FxHashSet};

#[derive(Default)]
pub struct Groups<P: Platform> {
	groups: im::HashMap<GroupId, Group<P>>,
	by_signer: im::HashMap<Address, GroupId>,
	by_order: im::HashMap<OrderHash, GroupId>,
}

impl<P: Platform> Groups<P> {
	pub fn get(&self, id: &GroupId) -> Option<Group<P>> {
		self.groups.get(id).cloned()
	}

	pub fn get_mut(&mut self, id: &GroupId) -> Option<&mut Group<P>> {
		self.groups.get_mut(id)
	}

	/// Returns a list of all group IDs that would be affected by inserting
	/// an order with the given signers. This is the list of groups that share at
	/// least one signer with the given order. Upon isertion all these groups
	/// will be merged into one.
	pub fn touched(&self, order: &PooledOrder<P>) -> Vec<GroupId> {
		let signers = order.signers();
		assert!(!signers.is_empty());
		let mut touched = FxHashSet::<GroupId>::default();

		for signer in signers {
			if let Some(gid) = self.by_signer.get(signer) {
				touched.insert(*gid);
			}
		}

		touched.into_iter().collect()
	}

	pub fn insert(&mut self, order: PooledOrder<P>) -> GroupId {
		if let Some(existing) = self.by_order.get(&order.hash()) {
			// already present
			return *existing;
		}

		let touched = self.touched(&order);

		match touched.len() {
			// new group with just this order
			0 => self.create_new_group(order),

			// insert into existing group and return its (possibly updated) id
			1 => {
				let gid = *touched.iter().next().expect("present");
				self.extend_group(gid, order)
			}

			// merge multiple groups by absorbing into the largest group
			_ => self.merge_groups(order, touched),
		}
	}
}

/// Insert - internal impl details
impl<P: Platform> Groups<P> {
	/// When an order does not have any signer overlap with existing groups,
	/// create a new group for it.
	fn create_new_group(&mut self, order: PooledOrder<P>) -> GroupId {
		let order_hash = order.hash();
		let signers = order.signers().clone();

		assert!(
			!signers.is_empty(),
			"signer-less order must be rejected at higher layers"
		);

		let new_group = Group::one(order);
		let gid = new_group.id();

		self.groups.insert(gid, new_group);
		self.by_order.insert(order_hash, gid);

		for s in signers {
			self.by_signer.insert(s, gid);
		}
		gid
	}

	/// When an order overlaps with exactly one existing group, insert it into
	/// that group. Returns the (possibly updated) GroupId of the group.
	fn extend_group(&mut self, gid: GroupId, order: PooledOrder<P>) -> GroupId {
		let order_hash = order.hash();
		let signers = order.signers().clone();

		assert!(
			!signers.is_empty(),
			"signer-less order must be rejected at higher layers"
		);

		self.get_mut(&gid).expect("group exists").insert(order);
		let gid_new = self.groups.get(&gid).expect("present").id();
		if gid_new == gid {
			self.by_order.insert(order_hash, gid);
			for a in signers {
				self.by_signer.insert(a, gid);
			}
			gid
		} else {
			let updated = self.groups.remove(&gid).expect("present");
			self.groups.insert(gid_new, updated);
			self.reindex_group(gid_new);
			gid_new
		}
	}

	fn merge_groups(
		&mut self,
		order: PooledOrder<P>,
		groups: Vec<GroupId>,
	) -> GroupId {
		assert!(!groups.is_empty());

		// we will collapse all groups into the largest one (smaller anchor if tie)
		let target_gid = *groups
			.iter()
			.max_by(|a, b| {
				let a_len = self.groups.get(a).map(|g| g.len()).expect("present");
				let b_len = self.groups.get(b).map(|g| g.len()).expect("present");
				a_len.cmp(&b_len).then_with(|| a.anchor().cmp(&b.anchor()))
			})
			.expect("touched is non-empty");

		let max_epoch = groups
			.iter()
			.max_by_key(|gid| gid.epoch())
			.expect("non-empty")
			.epoch();

		let others: Vec<Group<P>> = groups
			.into_iter()
			.filter(|gid| *gid != target_gid)
			.map(|gid| self.groups.remove(&gid).expect("present"))
			.collect();

		// Insert the new order into the target first (bridge).
		let target = self.groups.get_mut(&target_gid).expect("present");
		target.insert(order);

		// Union internals in-place.
		let GroupInner::Active(target_arc) = &target.0 else {
			unreachable!()
		};
		let mut target_inner = target_arc.write();
		let mut new_anchor = target_inner.id.anchor();

		for group in others {
			let GroupInner::Active(other_arc) = group.0 else {
				unreachable!()
			};
			let other = other_arc.read();

			for (h, o) in &other.orders {
				target_inner.orders.entry(*h).or_insert_with(|| o.clone());
			}
			for (&a, &rc) in &other.order_rc {
				*target_inner.order_rc.entry(a).or_insert(0) += rc;
				target_inner.adj.entry(a).or_default();
				if a < new_anchor {
					new_anchor = a;
				}
			}
			for (&(a, b), &rc) in &other.edge_rc {
				let entry = target_inner.edge_rc.entry((a, b)).or_insert(0);
				let was_zero = *entry == 0;
				*entry += rc;
				if was_zero && *entry > 0 {
					target_inner.adj.entry(a).or_default().insert(b);
					target_inner.adj.entry(b).or_default().insert(a);
				}
			}
		}

		// Finalize id: merge rule epoch = max_epoch + 1; anchor may shrink.
		target_inner.id = GroupId(new_anchor, max_epoch + 1);
		drop(target_inner);

		// Move entry if GroupId changed; then reindex once.
		let gid_new = self.groups.get(&target_gid).expect("present").id();
		let final_gid = if gid_new == target_gid {
			target_gid
		} else {
			let updated = self.groups.remove(&target_gid).expect("present");
			self.groups.insert(gid_new, updated);
			gid_new
		};

		self.reindex_group(final_gid);
		final_gid
	}

	// Reindex all signers and orders for a single group id.
	fn reindex_group(&mut self, gid: GroupId) {
		let g = self.groups.get(&gid).expect("present");
		let GroupInner::Active(inner) = &g.0 else {
			unreachable!("cannot reindex tombstone group");
		};

		let inner = inner.read();
		// reindex orders
		for &h in inner.orders.keys() {
			self.by_order.insert(h, gid);
		}

		// reindex signers
		for &s in inner.order_rc.keys() {
			self.by_signer.insert(s, gid);
		}
	}
}

#[cfg(test)]
mod tests {
	use {
		super::{super::tests::make_tx, *},
		crate::test_utils::*,
		itertools::Itertools,
	};

	#[rblib_test(Ethereum, Optimism)]
	fn insert_into_empty_creates_one_group<P: TestablePlatform>() {
		let tx1 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let order1 = Order::Transaction(tx1);
		let po1: PooledOrder<P> = order1.into();

		let mut groups = Groups::<P>::default();
		let gid = groups.insert(po1);

		assert_eq!(groups.groups.len(), 1);
		assert_eq!(gid.epoch(), 0);
		assert_eq!(gid.anchor(), FundedAccounts::signer(0).address());
		assert_eq!(groups.by_order.len(), 1);
		assert_eq!(groups.by_signer.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_same_signer_set_keeps_groupid<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		// Bundle with signers 0 and 1
		let tx0a = make_tx::<P>(&signers[0], 0);
		let tx1a = make_tx::<P>(&signers[1], 0);
		let bundle1 = FlashbotsBundle::<P>::default()
			.with_transaction(tx0a)
			.with_transaction(tx1a);
		let po1: PooledOrder<P> = Order::Bundle(bundle1).into();

		// Another bundle with the same signer set (different nonces)
		let tx0b = make_tx::<P>(&signers[0], 1);
		let tx1b = make_tx::<P>(&signers[1], 1);
		let bundle2 = FlashbotsBundle::<P>::default()
			.with_transaction(tx0b)
			.with_transaction(tx1b);
		let po2: PooledOrder<P> = Order::Bundle(bundle2).into();

		let mut groups = Groups::<P>::default();
		let gid1 = groups.insert(po1);
		let gid2 = groups.insert(po2);

		// Same anchor and epoch; GroupId unchanged
		assert_eq!(gid1, gid2);
		assert_eq!(gid2.anchor(), signers[0].address());
		assert_eq!(gid2.epoch(), 0);
		assert_eq!(groups.groups.len(), 1);
		assert_eq!(groups.by_order.len(), 2);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_with_new_larger_signers_keeps_groupid<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		// Start with bundle {signer 1, signer 2} (anchor = 1)
		let tx1a = make_tx::<P>(&signers[0], 0);
		let tx2a = make_tx::<P>(&signers[1], 0);
		let bundle1 = FlashbotsBundle::<P>::default()
			.with_transaction(tx1a)
			.with_transaction(tx2a);
		let po1: PooledOrder<P> = Order::Bundle(bundle1).into();

		// Extend with bundle {signer 2, signer 3} → anchor stays 1
		let tx2b = make_tx::<P>(&signers[1], 1);
		let tx3b = make_tx::<P>(&signers[2], 0);
		let bundle2 = FlashbotsBundle::<P>::default()
			.with_transaction(tx2b)
			.with_transaction(tx3b);

		let po2: PooledOrder<P> = Order::Bundle(bundle2).into();

		let mut groups = Groups::<P>::default();
		let gid1 = groups.insert(po1);
		let gid2 = groups.insert(po2);

		assert_eq!(gid1, gid2);
		assert_eq!(gid2.anchor(), signers[0].address());
		assert_eq!(gid2.epoch(), 0);
		assert_eq!(groups.groups.len(), 1);
		// signers now include 1,2,5
		assert_eq!(groups.by_signer.len(), 3);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_with_smaller_new_signer_changes_groupid<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// Sorted by address to make anchor deterministic
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		// Start with bundle {s1, s2} (anchor = s1)
		let tx1a = make_tx::<P>(&signers[1], 0);
		let tx2a = make_tx::<P>(&signers[2], 0);
		let bundle1 = FlashbotsBundle::<P>::default()
			.with_transaction(tx1a)
			.with_transaction(tx2a);
		let po1: PooledOrder<P> = Order::Bundle(bundle1).into();

		let mut groups = Groups::<P>::default();
		let gid_before = groups.insert(po1);

		// Extend with bundle {s2, s0} → anchor shrinks to s0, epoch +1
		let tx2b = make_tx::<P>(&signers[2], 1);
		let tx0b = make_tx::<P>(&signers[0], 0);
		let bundle2 = FlashbotsBundle::<P>::default()
			.with_transaction(tx2b)
			.with_transaction(tx0b);
		let po2: PooledOrder<P> = Order::Bundle(bundle2).into();

		let gid_after = groups.insert(po2);

		assert_ne!(gid_before, gid_after);
		assert_eq!(gid_after.anchor(), signers[0].address());
		assert_eq!(gid_after.epoch(), gid_before.epoch() + 1);
		assert_eq!(groups.groups.len(), 1);
		assert_eq!(groups.by_order.len(), 2);
		assert!(groups.by_signer.contains_key(&signers[0].address()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn merge_multiple_groups_anchor_unchanged<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2 < s3 < s4 (sorted by address)
		let signers = (0..5)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		// Group A: {s1, s2} (anchor s1)
		let tx1a = make_tx::<P>(&signers[1], 0);
		let tx2a = make_tx::<P>(&signers[2], 0);
		let bundle_a = FlashbotsBundle::<P>::default()
			.with_transaction(tx1a)
			.with_transaction(tx2a);
		let po_a: PooledOrder<P> = Order::Bundle(bundle_a).into();

		// Group B: {s3, s4} (anchor s3)
		let tx3b = make_tx::<P>(&signers[3], 0);
		let tx4b = make_tx::<P>(&signers[4], 0);
		let bundle_b = FlashbotsBundle::<P>::default()
			.with_transaction(tx3b)
			.with_transaction(tx4b);
		let po_b: PooledOrder<P> = Order::Bundle(bundle_b).into();

		let mut groups = Groups::<P>::default();
		let gid_a0 = groups.insert(po_a);
		let gid_b0 = groups.insert(po_b);

		assert_eq!(groups.groups.len(), 2);

		// Bridge order touches both A (s2) and B (s3): merge; no smaller signer
		// introduced
		let tx2c = make_tx::<P>(&signers[2], 1);
		let tx3c = make_tx::<P>(&signers[3], 1);
		let bridge = FlashbotsBundle::<P>::default()
			.with_transaction(tx2c)
			.with_transaction(tx3c);
		let po_bridge: PooledOrder<P> = Order::Bundle(bridge).into();

		let gid_final = groups.insert(po_bridge);

		// One group remains, anchor unchanged (s1), but merges bump epoch → GroupId
		// changes
		assert_eq!(groups.groups.len(), 1);
		assert_eq!(gid_final.anchor(), signers[1].address());
		assert_eq!(gid_final.epoch(), 1);
		assert_ne!(gid_final, gid_a0);
		assert_ne!(gid_final, gid_b0);

		// Indices cover all signers and orders
		for acct in [&signers[1], &signers[2], &signers[3], &signers[4]] {
			assert_eq!(groups.by_signer.get(&acct.address()), Some(&gid_final));
		}
		assert_eq!(groups.by_order.len(), 3);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn merge_multiple_groups_anchor_changes<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2 < s3 < s4 < s5 (sorted by address)
		let signers = (0..6)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		// Group A: {s2, s3} (anchor s2)
		let tx2a = make_tx::<P>(&signers[2], 0);
		let tx3a = make_tx::<P>(&signers[3], 0);
		let bundle_a = FlashbotsBundle::<P>::default()
			.with_transaction(tx2a)
			.with_transaction(tx3a);
		let po_a: PooledOrder<P> = Order::Bundle(bundle_a).into();

		// Group B: {s4, s5} (disjoint from A; anchor s4)
		let tx4b = make_tx::<P>(&signers[4], 0);
		let tx5b = make_tx::<P>(&signers[5], 0);
		let bundle_b = FlashbotsBundle::<P>::default()
			.with_transaction(tx4b)
			.with_transaction(tx5b);
		let po_b: PooledOrder<P> = Order::Bundle(bundle_b).into();

		let mut groups = Groups::<P>::default();
		let gid_a0 = groups.insert(po_a);
		let gid_b0 = groups.insert(po_b);

		assert_eq!(groups.groups.len(), 2);

		// Bridge touches both A (s3) and B (s4), and introduces new smallest signer
		// s0 → anchor shrinks
		let tx3c = make_tx::<P>(&signers[3], 1);
		let tx4c = make_tx::<P>(&signers[4], 1);
		let tx0c = make_tx::<P>(&signers[0], 0);
		let bridge = FlashbotsBundle::<P>::default()
			.with_transaction(tx3c)
			.with_transaction(tx4c)
			.with_transaction(tx0c);
		let po_bridge: PooledOrder<P> = Order::Bundle(bridge).into();

		let gid_final = groups.insert(po_bridge);

		assert_eq!(groups.groups.len(), 1);
		assert_eq!(gid_final.anchor(), signers[0].address());
		assert_eq!(gid_final.epoch(), 1);
		assert_ne!(gid_final, gid_a0);
		assert_ne!(gid_final, gid_b0);

		// Indices include all signers from both groups plus s0
		for acct in [
			&signers[0],
			&signers[2],
			&signers[3],
			&signers[4],
			&signers[5],
		] {
			assert_eq!(groups.by_signer.get(&acct.address()), Some(&gid_final));
		}
		assert_eq!(groups.by_order.len(), 3);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_dedup_same_order_hash<P: TestablePlatform>() {
		// One signer, sorted by address
		let addrs = [FundedAccounts::signer(0).address()]
			.into_iter()
			.sorted()
			.collect::<Vec<_>>();
		let signer0 = FundedAccounts::by_address(addrs[0]).unwrap();

		let tx = make_tx::<P>(&signer0, 0);
		let order = Order::Transaction(tx.clone());
		let po: PooledOrder<P> = order.into();
		let h = po.hash();

		let mut groups = Groups::<P>::default();

		let gid1 = groups.insert(po.clone());
		assert_eq!(groups.groups.len(), 1);
		assert_eq!(gid1.anchor(), addrs[0]);
		assert_eq!(gid1.epoch(), 0);
		assert_eq!(groups.by_order.len(), 1);
		assert_eq!(groups.by_signer.len(), 1);

		// Re-insert the exact same order (same hash) → no-op
		let gid2 = groups.insert(po.clone());
		assert_eq!(gid1, gid2);
		assert_eq!(groups.groups.len(), 1);
		assert_eq!(groups.by_order.len(), 1);
		assert_eq!(groups.by_signer.len(), 1);
		assert_eq!(groups.by_order.get(&h), Some(&gid1));
	}
}
