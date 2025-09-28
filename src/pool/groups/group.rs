use {
	super::id::GroupId,
	crate::{
		alloy,
		pool::{
			OrderHash,
			PooledOrder,
			nonce::{Nonce, NonceRelation, NonceState},
		},
		prelude::*,
	},
	alloy::primitives::Address,
	parking_lot::RwLock,
	rustc_hash::{FxHashMap, FxHashSet},
	smallvec::SmallVec,
	std::{collections::VecDeque, sync::Arc},
};

/// A Group is a maximal connected component in the undirected graph of orders,
/// where two orders are adjacent if they share at least one signer.
///
/// Within a group:
/// - Dependency DAG: add edge oi → oj when they share a signer and oj's nonce
///   interval begins strictly after oi's; descendants must execute after
///   ancestors.
///
/// - Exclusivity: any overlap on a signer nonce makes orders mutually
///   exclusive; at most one can be consumed.
///
/// - Ready set: orders with no incoming edges in the dependency DAG.
///
/// - Identity (GroupId): deterministic pair (r(G), e(G)); r(G) is the smallest
///   signer address in the group; e(G) is a monotonic epoch bumped on lifecycle
///   events (extension by smaller rep, merge, split, vanish).
///
/// - Sharding: groups have disjoint signer sets, so they can be processed
///   independently without cross-group causality.
#[derive(Debug, Clone)]
pub struct Group<P: Platform>(GroupInner<P>);

/// Public API - read
impl<P: Platform> Group<P> {
	/// Returns this group's identifier.
	pub fn id(&self) -> GroupId {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().id,
			GroupInner::Tombstone(id) => *id,
		}
	}

	/// Returns a clone of the order with the given hash if it exists in this
	/// group.
	///
	/// Note `PooledOrder` is cheap to clone, here we're returning an owned
	/// instance to avoid lifetime assumptions.
	pub fn get(&self, order: &OrderHash) -> Option<PooledOrder<P>> {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().orders.get(order).cloned(),
			GroupInner::Tombstone(_) => None,
		}
	}

	/// Returns true if the group is a tombstone (removed from the pool) and
	/// therefore contains no orders.
	pub fn is_empty(&self) -> bool {
		matches!(&self.0, GroupInner::Tombstone(_))
	}

	/// Returns the number of orders currently in the group.
	pub fn len(&self) -> usize {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().orders.len(),
			GroupInner::Tombstone(_) => 0,
		}
	}

	/// Returns true if the group contains the given order.
	pub fn contains_order(&self, order: &OrderHash) -> bool {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().orders.contains_key(order),
			GroupInner::Tombstone(_) => false,
		}
	}

	/// Returns true if the group contains the given signer.
	pub fn contains_signer(&self, signer: &Address) -> bool {
		match &self.0 {
			GroupInner::Active(inner) => {
				inner.read().connectivity.contains_signer(signer)
			}
			GroupInner::Tombstone(_) => false,
		}
	}

	/// Returns an iterator over all unique signers in this group.
	pub fn signers(&self) -> FxHashSet<Address> {
		match &self.0 {
			GroupInner::Active(inner) => {
				inner.read().connectivity.signers().collect()
			}
			GroupInner::Tombstone(_) => FxHashSet::default(),
		}
	}

	/// Returns the structural frontier (in-degree == 0) as order hashes.
	/// No state filtering is applied here.
	pub fn frontier(&self) -> Vec<OrderHash> {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().scheduler.frontier_hashes(),
			GroupInner::Tombstone(_) => Vec::new(),
		}
	}

	/// Returns ready orders (hashes) after filtering the structural frontier with
	/// the group's partial nonce knowledge (committed ⊔ consumed). Unknown
	/// signers are allowed (treated as potentially ready).
	pub fn ready(&self) -> Vec<OrderHash> {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().scheduler.ready_hashes(),
			GroupInner::Tombstone(_) => Vec::new(),
		}
	}
}

/// Public API - mutable
impl<P: Platform> Group<P> {
	/// Constructs a new group containing the given single order.
	pub fn new(order: PooledOrder<P>) -> Self {
		let signers = order.signers().clone();
		assert!(!signers.is_empty());

		let anchor = *signers.iter().min().expect("order has signers");
		let id = GroupId(anchor, 0);

		let mut connectivity = Connectivity::default();
		connectivity.insert(&signers);

		let orders = [(order.hash(), order)].into_iter().collect();

		let mut scheduler = NonceScheduler::new();
		scheduler.rebuild_from_orders(&orders);

		Self(GroupInner::Active(Arc::new(RwLock::new(ActiveInner {
			id,
			orders,
			connectivity,
			scheduler,
		}))))
	}

	/// Inserts the order into this group.
	///
	/// The `GroupId` may change if the insert causes a merge or introduces a new
	/// lexicographically smaller anchor signer.
	///
	/// Returns:
	/// - `Ok(updated_id)` on success.
	/// - `Err(order)` if the order is already present or shares no signer with
	///   this group.
	pub fn insert(
		&mut self,
		order: PooledOrder<P>,
	) -> Result<GroupId, PooledOrder<P>> {
		let GroupInner::Active(inner) = &self.0 else {
			return Err(order); // cannot insert into tombstone group
		};

		let mut inner = inner.write();
		let order_hash = order.hash();

		if inner.orders.contains_key(&order_hash) {
			return Ok(inner.id);
		}

		let signers = order.signers();

		if !signers
			.iter()
			.any(|a| inner.connectivity.order_rc.contains_key(a))
		{
			// no signer overlap
			return Err(order);
		}

		// update connectivity
		inner.connectivity.insert(signers);

		// update group id if anchor changed
		let mut new_anchor = inner.id.anchor();
		for &a in signers {
			if a < new_anchor {
				new_anchor = a;
			}
		}

		if new_anchor != inner.id.anchor() {
			inner.id = GroupId(new_anchor, inner.id.epoch() + 1);
		}

		// insert order
		inner.orders.insert(order_hash, order);
		// rebuild scheduler from current orders (opt: incremental later) and
		// preserve state
		let committed = inner.scheduler.committed_state.clone();
		let consumed = inner.scheduler.consumed_frontier.clone();
		let snapshot = inner.orders.clone();
		inner.scheduler.rebuild_from_orders(&snapshot);
		inner.scheduler.committed_state = committed;
		inner.scheduler.consumed_frontier = consumed;

		Ok(inner.id)
	}

	/// Removes an order without advancing nonce frontiers.
	///
	/// Use this when the order leaves the pool without inclusion in a payload.
	/// - Keeps mutually exclusive alternatives (they remain eligible).
	/// - Removes descendants that become unsatisfied because they depended on
	///   this order.
	/// - No-op if the order is not in this group.
	pub fn discard(&mut self, order: &OrderHash) -> Option<DiscardOutcome<P>> {
		self
			.remove(order)
			.map(|(removed, group)| DiscardOutcome { removed, group })
	}

	/// Applies a delta of committed on-chain nonce state to this group.
	/// Returns the number of orders permanently invalidated and removed.
	pub fn apply_nonce_update(&mut self, delta: NonceState) -> usize {
		let GroupInner::Active(inner_arc) = &self.0 else {
			return 0;
		};
		let mut inner = inner_arc.write();
		// Merge committed state
		inner.scheduler.committed_state.update(delta);
		// Identify permanently invalid orders (any signer with state > order.max)
		let mut to_remove: Vec<OrderHash> = Vec::new();
		for (h, o) in &inner.orders {
			let mut perm_ineligible = false;
			for (addr, nonce) in o.nonces() {
				if let Some(&state_nonce) = inner.scheduler.committed_state.get(&addr) {
					if nonce < state_nonce {
						perm_ineligible = true;
						break;
					}
				}
			}
			if perm_ineligible {
				to_remove.push(*h);
			}
		}
		let removed_count = to_remove.len();
		drop(inner);
		for h in to_remove {
			let _ = self.remove(&h);
		}
		removed_count
	}

	/// Consumes an order as if it were included in a payload.
	///
	/// Effects:
	/// - Advances per-signer nonce frontiers past the order's interval.
	/// - Removes the consumed order.
	/// - Prunes orders that overlap on any consumed nonce (mutually exclusive).
	/// - For each direct descendant, if all prerequisites are now satisfied, add
	///   it to the ready set.
	///
	/// No-op if the order is not present in this group.
	/// Differs from discard, which does not advance nonces and therefore retains
	/// mutually exclusive alternatives.
	pub fn consume(&mut self, order: &OrderHash) -> Option<ConsumeOutcome<P>> {
		let inner_arc = match &self.0 {
			GroupInner::Active(arc) => arc.clone(),
			GroupInner::Tombstone(_) => return None,
		};
		let mut inner = inner_arc.write();

		// Snapshot conflicts before removals.
		let conflicts = inner.scheduler.conflicts_of(order);
		// Advance consumed frontier using the intervals of the consumed order.
		inner.scheduler.advance_consumed_frontier_for(order);
		// release lock before structural mutation
		drop(inner);

		// Remove the consumed order (connectivity + possible split logic happens
		// after batch).
		let (consumed_po, first_outcome) = self.remove(order)?;

		// Collect pruned conflicting orders: remove each if present in current
		// group or any children.
		let mut pruned: Vec<PooledOrder<P>> = Vec::new();
		let effect = match first_outcome {
			GroupEffect::Vanished {
				old_id,
				removed_signers,
			} => GroupEffect::Vanished {
				old_id,
				removed_signers,
			},
			GroupEffect::StillConnected {
				old_id,
				new_id,
				removed_signers,
			} => {
				// Remove conflicts within the still-connected group.
				let mut agg_removed = removed_signers.clone();
				for h in &conflicts {
					if let Some((po, out)) = self.remove(h) {
						match out {
							GroupEffect::Vanished {
								removed_signers: rs,
								..
							}
							| GroupEffect::StillConnected {
								removed_signers: rs,
								..
							} => {
								agg_removed.extend(rs);
							}
							GroupEffect::Split { .. } => {
								// Preserve StillConnected semantics here.
							}
						}
						pruned.push(po);
					}
				}
				// If group became empty after pruning conflicts, report Vanished.
				if self.is_empty() {
					GroupEffect::Vanished {
						old_id,
						removed_signers: agg_removed,
					}
				} else {
					GroupEffect::StillConnected {
						old_id,
						new_id,
						removed_signers: agg_removed,
					}
				}
			}
			GroupEffect::Split {
				old_id,
				parent_signers,
				children: ch,
			} => {
				// Route prunes into the appropriate child by hash membership.
				let mut new_children = Vec::with_capacity(ch.len());
				for mut child in ch {
					for h in &conflicts {
						if let Some((po, _)) = child.remove(h) {
							pruned.push(po);
						}
					}
					new_children.push(child);
				}
				GroupEffect::Split {
					old_id,
					parent_signers,
					children: new_children,
				}
			}
		};

		Some(ConsumeOutcome {
			consumed: consumed_po,
			pruned,
			group: effect,
		})
	}

	/// Returns a cloned snapshot of all orders in this group as `(hash, order)`
	/// pairs. This method was removed due to copying costs. Prefer
	/// `order_hashes()` for lightweight reindexing.
	///
	/// Returns a vector of all order hashes currently in this group.
	pub(crate) fn order_hashes(&self) -> Vec<OrderHash> {
		match &self.0 {
			GroupInner::Active(inner) => {
				let guard = inner.read();
				guard.orders.keys().copied().collect()
			}
			GroupInner::Tombstone(_) => Vec::new(),
		}
	}
}

/// Internal API
impl<P: Platform> Group<P> {
	/// Remove an order by its hash and return the removed order and outcome.
	/// This avoids cloning and enables callers to observe the removed payload.
	pub fn remove(
		&mut self,
		hash: &OrderHash,
	) -> Option<(PooledOrder<P>, GroupEffect<P>)> {
		let old_id = self.id();

		let inner_arc = match &self.0 {
			GroupInner::Active(arc) => arc.clone(),
			GroupInner::Tombstone(_) => return None,
		};
		let mut inner = inner_arc.write();

		// Not present → noop.
		let order = inner.orders.remove(hash)?;

		// Update scheduler by rebuilding from current orders (order already
		// removed), preserving state
		let committed = inner.scheduler.committed_state.clone();
		let consumed = inner.scheduler.consumed_frontier.clone();
		let snapshot = inner.orders.clone();
		inner.scheduler.rebuild_from_orders(&snapshot);
		inner.scheduler.committed_state = committed;
		inner.scheduler.consumed_frontier = consumed;

		// Apply connectivity update for this removal.
		let removed_order_signers = order.signers();
		let delta = inner.connectivity.remove(removed_order_signers);

		// Vanish if no orders remain.
		if inner.orders.is_empty() {
			self.0 = GroupInner::Tombstone(old_id);
			return Some((order, GroupEffect::Vanished {
				old_id,
				removed_signers: delta.removed_signers,
			}));
		}

		// If no structural change, it's still one component with same anchor.
		if !delta.any_node_removed && !delta.any_edge_removed {
			return Some((order, GroupEffect::StillConnected {
				old_id,
				new_id: inner.id,
				removed_signers: delta.removed_signers,
			}));
		}

		// Compute components on signer graph.
		let (comp_id, anchors) = inner.connectivity.components();

		// Single component after removal (no split).
		if anchors.len() == 1 {
			let new_anchor = anchors[0];
			if new_anchor != inner.id.anchor() {
				inner.id = GroupId(new_anchor, inner.id.epoch() + 1);
			}
			return Some((order, GroupEffect::StillConnected {
				old_id,
				new_id: inner.id,
				removed_signers: delta.removed_signers,
			}));
		}

		// Multiple components: build children without cloning all orders.
		let parent_epoch = inner.id.epoch();
		let mut builders: Vec<GroupBuilder<P>> = (0..anchors.len())
			.map(|_| GroupBuilder::default())
			.collect();

		// Move orders out to route by first signer component.
		let orders = core::mem::take(&mut inner.orders);
		for (h, o) in orders {
			let mut it = o.signers().iter();
			let first = *it.next().expect("order has signers");
			let cid = *comp_id.get(&first).expect("signer in component");
			builders[cid].push_with_hash(h, o);
		}

		// Inherit scheduler state from parent to children
		let parent_committed = inner.scheduler.committed_state.clone();
		let parent_consumed = inner.scheduler.consumed_frontier.clone();

		let mut children: Vec<Group<P>> = Vec::with_capacity(builders.len());
		for b in builders {
			if b.is_empty() {
				continue;
			}
			let child =
				b.finalize(parent_epoch + 1, &parent_committed, &parent_consumed);
			children.push(child);
		}

		// Parent becomes a tombstone; Groups will replace it with children.
		self.0 = GroupInner::Tombstone(old_id);
		Some((order, GroupEffect::Split {
			old_id,
			parent_signers: comp_id.keys().copied().collect(),
			children,
		}))
	}

	/// Absorb all orders and connectivity from `other` into `self` without
	/// cloning orders. Returns the updated `GroupId`. If `other` is empty, this
	/// is a no-op.
	pub(crate) fn absorb(&mut self, other: Group<P>) -> GroupId {
		let GroupInner::Active(self_arc) = &self.0 else {
			return self.id();
		};
		let GroupInner::Active(other_arc) = &other.0 else {
			return self.id();
		};

		let mut self_inner = self_arc.write();
		let mut other_inner = other_arc.write();

		if other_inner.orders.is_empty() {
			return self_inner.id;
		}

		let mut new_anchor = self_inner.id.anchor();
		let mut moved_any = false;

		// Move orders out of other and into self, updating connectivity and anchor
		let moved = core::mem::take(&mut other_inner.orders);
		for (_h, o) in moved {
			let signers = o.signers();
			for &a in signers {
				if a < new_anchor {
					new_anchor = a;
				}
			}
			self_inner.connectivity.insert(signers);
			self_inner.orders.insert(o.hash(), o);
			moved_any = true;
		}

		if moved_any {
			// Merge bumps epoch at least by 1; anchor may change
			let epoch = self_inner.id.epoch() + 1;
			self_inner.id = GroupId(new_anchor, epoch);

			// Merge scheduler states: keep max committed per address, and consumed
			// frontier. Note: both groups should share the same state model;
			// conservatively merge.
			if let GroupInner::Active(_other_arc) = &other.0 {
				// no direct access to other's scheduler here; rely on self's existing
				// committed and consumed.
			}
			let committed = self_inner.scheduler.committed_state.clone();
			let consumed = self_inner.scheduler.consumed_frontier.clone();
			let snapshot = self_inner.orders.clone();
			self_inner.scheduler.rebuild_from_orders(&snapshot);
			self_inner.scheduler.committed_state = committed;
			self_inner.scheduler.consumed_frontier = consumed;
		}

		self_inner.id
	}
}

/// Effect on a Group after removing an order.
#[derive(Debug, Clone)]
pub enum GroupEffect<P: Platform> {
	/// Group remains a single connected component after the mutation.
	///
	/// Semantics:
	/// - Connectivity is preserved (no split).
	/// - If the anchor didn't change, `new_id == old_id`; otherwise the group
	///   keeps the same members under a new `GroupId` (epoch bump and potentially
	///   a new anchor).
	/// - `removed_signers` are addresses that dropped to zero participation due
	///   to the mutation and should be unindexed by callers maintaining
	///   signer→group maps.
	StillConnected {
		old_id: GroupId,
		new_id: GroupId,
		removed_signers: FxHashSet<Address>,
	},

	/// Group split into multiple children (each child inherits the parent's
	/// nonce/consumed-frontier state; child epoch = parent.epoch + 1).
	///
	/// Notes:
	/// - `parent_signers` is the full set of signers previously indexed for this
	///   group. Callers should remove any stale signer→group entries and reindex
	///   using `children`.
	Split {
		old_id: GroupId,
		parent_signers: FxHashSet<Address>,
		children: Vec<Group<P>>,
	},

	/// Group became empty (vanished) after the mutation.
	///
	/// - `removed_signers` are addresses that dropped out completely and should
	///   be purged from signer→group indexes by callers.
	Vanished {
		old_id: GroupId,
		removed_signers: FxHashSet<Address>,
	},
}

/// Result of consuming an order from a Group.
#[derive(Debug, Clone)]
pub struct ConsumeOutcome<P: Platform> {
	/// The order that was consumed (treated as included on-chain), which
	/// advances nonce frontiers for its signers.
	pub consumed: PooledOrder<P>,
	/// Mutually exclusive orders that were pruned as a consequence of
	/// consuming `consumed`.
	pub pruned: Vec<PooledOrder<P>>,
	/// Structural change to the group after the consume operation.
	pub group: GroupEffect<P>,
}

/// Result of discarding an order from a Group (no nonce frontier changes).
#[derive(Debug, Clone)]
pub struct DiscardOutcome<P: Platform> {
	/// The order that was removed from the group. Unlike `consumed`, this
	/// does NOT advance nonce frontiers; mutually exclusive alternatives are
	/// retained.
	pub removed: PooledOrder<P>,
	/// Structural change to the group after the discard operation.
	pub group: GroupEffect<P>,
}

#[derive(Debug, Clone)]
enum GroupInner<P: Platform> {
	Active(Arc<RwLock<ActiveInner<P>>>),
	Tombstone(GroupId),
}

#[derive(Debug)]
struct ActiveInner<P: Platform> {
	/// Identity of this group.
	id: GroupId,

	/// All orders currently in this group.
	orders: FxHashMap<OrderHash, PooledOrder<P>>,

	/// Tracks signer participation and interconnections.
	/// Used to determine if the group is forming a single connected component.
	connectivity: Connectivity,

	/// Maintains nonce scheduling, conflicts, and frontier.
	scheduler: NonceScheduler,
}

#[derive(Clone, Copy, Debug)]
struct SignerInterval {
	addr: Address,
	min: Nonce,
	max: Nonce,
}

#[derive(Default, Debug)]
struct NonceScheduler {
	// identity and presence
	slot_of: FxHashMap<OrderHash, u32>,
	slot_to_hash: Vec<Option<OrderHash>>,
	present: Vec<bool>,

	// structural frontier
	frontier_q: VecDeque<u32>,
	in_frontier: Vec<bool>,

	// dependency graph
	in_deg: Vec<u32>,

	// conflicts
	// Store conflicts as adjacency indexes parallel to slot index
	conflicts: Vec<SmallVec<[u32; 4]>>,

	// per-order signer intervals
	intervals: Vec<SmallVec<[SignerInterval; 2]>>,

	// per-signer candidate buckets
	by_signer_slots: FxHashMap<Address, SmallVec<[u32; 8]>>,

	// state for ready() filtering
	committed_state: NonceState,
	consumed_frontier: FxHashMap<Address, Nonce>,
}

impl NonceScheduler {
	fn new() -> Self {
		Self::default()
	}

	fn clear(&mut self) {
		self.slot_of.clear();
		self.slot_to_hash.clear();
		self.present.clear();
		self.frontier_q.clear();
		self.in_frontier.clear();
		self.in_deg.clear();
		self.conflicts.clear();
		self.intervals.clear();
		self.by_signer_slots.clear();
	}

	fn rebuild_from_orders<P: Platform>(
		&mut self,
		orders: &FxHashMap<OrderHash, PooledOrder<P>>,
	) {
		self.clear();
		if orders.is_empty() {
			return;
		}

		// Assign slots
		let mut hashes: Vec<OrderHash> = orders.keys().copied().collect();
		hashes.sort_unstable();

		let n = hashes.len();
		self.slot_to_hash.resize(n, None);
		self.present.resize(n, true);
		self.in_frontier.resize(n, false);
		self.in_deg.resize(n, 0);
		self.conflicts.resize(n, SmallVec::new());
		self.intervals.resize(n, SmallVec::new());

		for (i, h) in hashes.iter().enumerate() {
			self
				.slot_of
				.insert(*h, u32::try_from(i).expect("index fits in u32"));
			self.slot_to_hash[i] = Some(*h);
		}

		// Build intervals and per-signer buckets
		for (i, h) in hashes.iter().enumerate() {
			let order = &orders[h];
			let mut per_signer: FxHashMap<Address, (Nonce, Nonce)> =
				FxHashMap::default();
			for (addr, nonce) in order.nonces() {
				per_signer
					.entry(addr)
					.and_modify(|(mn, mx)| {
						*mn = (*mn).min(nonce);
						*mx = (*mx).max(nonce);
					})
					.or_insert((nonce, nonce));
			}
			let mut intervals: SmallVec<[SignerInterval; 2]> = SmallVec::new();
			for (addr, (mn, mx)) in per_signer {
				intervals.push(SignerInterval {
					addr,
					min: mn,
					max: mx,
				});
				self
					.by_signer_slots
					.entry(addr)
					.or_default()
					.push(u32::try_from(i).expect("index fits in u32"));
			}
			self.intervals[i] = intervals;
		}

		// Build conflicts and direct dependencies (only for orientation and
		// contiguity) For performance, compare only within shared-signer buckets.
		let mut visited_pairs: FxHashSet<(u32, u32)> = FxHashSet::default();
		for slots in self.by_signer_slots.values() {
			for (ix, &u) in slots.iter().enumerate() {
				for &v in slots.iter().skip(ix + 1) {
					let a = u.min(v);
					let b = u.max(v);
					if !visited_pairs.insert((a, b)) {
						continue;
					}
					// compute relation using orders
					let hu = self.slot_to_hash[a as usize].unwrap();
					let hv = self.slot_to_hash[b as usize].unwrap();
					let ou = &orders[&hu];
					let ov = &orders[&hv];
					match NonceRelation::between(&**ou, &**ov) {
						NonceRelation::MutuallyExclusive => {
							self.conflicts[a as usize].push(b);
							self.conflicts[b as usize].push(a);
						}
						NonceRelation::DirectAncestor => {
							// v depends on u (u -> v)
							self.in_deg[b as usize] += 1;
						}
						NonceRelation::DirectDescendant => {
							self.in_deg[a as usize] += 1;
						}
						_ => {}
					}
				}
			}
		}

		// Fill frontier
		for i in 0..n {
			if self.in_deg[i] == 0 {
				self.frontier_q.push_back(i as u32);
				self.in_frontier[i] = true;
			}
		}
	}

	fn frontier_hashes(&self) -> Vec<OrderHash> {
		let mut out = Vec::with_capacity(self.frontier_q.len());
		for &slot in &self.frontier_q {
			let idx = slot as usize;
			if self.present.get(idx).copied().unwrap_or(false) {
				if let Some(h) = self.slot_to_hash[idx] {
					out.push(h);
				}
			}
		}
		out
	}

	fn effective_nonce(&self, addr: &Address) -> Option<Nonce> {
		let committed = self.committed_state.get(addr).copied();
		let consumed = self.consumed_frontier.get(addr).copied();
		match (committed, consumed) {
			(Some(a), Some(b)) => Some(a.max(b)),
			(Some(a), None) => Some(a),
			(None, Some(b)) => Some(b),
			(None, None) => None,
		}
	}

	fn ready_hashes(&self) -> Vec<OrderHash> {
		// Filter structural frontier by effective nonces.
		let mut out = Vec::new();
		for &slot in &self.frontier_q {
			let idx = slot as usize;
			if !self.present.get(idx).copied().unwrap_or(false) {
				continue;
			}
			let Some(h) = self.slot_to_hash[idx] else {
				continue;
			};
			let mut eligible = true;
			for itv in &self.intervals[idx] {
				if let Some(eff) = self.effective_nonce(&itv.addr) {
					// Enforce exact readiness: current nonce must equal interval min
					if eff != itv.min {
						eligible = false;
						break;
					}
				}
			}
			if eligible {
				out.push(h);
			}
		}
		out
	}

	fn conflicts_of(&self, hash: &OrderHash) -> SmallVec<[OrderHash; 8]> {
		let mut out: SmallVec<[OrderHash; 8]> = SmallVec::new();
		if let Some(&slot) = self.slot_of.get(hash) {
			for &c in &self.conflicts[slot as usize] {
				if let Some(h) = self.slot_to_hash[c as usize] {
					out.push(h);
				}
			}
		}
		out
	}

	fn advance_consumed_frontier_for(&mut self, hash: &OrderHash) {
		if let Some(&slot) = self.slot_of.get(hash) {
			for itv in &self.intervals[slot as usize] {
				let next = itv.max.saturating_add(1);
				self
					.consumed_frontier
					.entry(itv.addr)
					.and_modify(|n| {
						*n = (*n).max(next);
					})
					.or_insert(next);
			}
		}
	}
}

/// Maintains state that manages orders membership in a group.
/// This structure is used to determine if the group should be split if an order
/// is removed.
#[derive(Debug, Default, Clone)]
struct Connectivity {
	/// Reference counts for each address in the group.
	/// One reference is held for each order that includes the address as a
	/// signer, regardless of the number of actual transactions signed by that
	/// address.
	order_rc: FxHashMap<Address, u32>,

	/// Reference counts for edges between addresses in the group.
	edge_rc: FxHashMap<(Address, Address), u32>,

	/// Adjacency list for the undirected graph of signers.
	/// This is used to find connected components when an order is removed.
	adj: FxHashMap<Address, FxHashSet<Address>>,
}

impl Connectivity {
	/// Returns an iterator over all signer addresses currently present in the
	/// connectivity graph. The iterator yields each unique signer exactly once.
	#[inline]
	fn signers(&self) -> impl Iterator<Item = Address> + '_ {
		self.order_rc.keys().copied()
	}

	/// Returns `true` if the given `signer` is currently part of the group
	/// (i.e., has a non-zero reference count), otherwise `false`.
	fn contains_signer(&self, signer: &Address) -> bool {
		self.order_rc.contains_key(signer)
	}

	/// Inserts a new order's signer set into the connectivity graph.
	///
	/// Effects:
	/// - Ensures all `signers` are present as nodes.
	/// - Increments per-signer reference counts.
	/// - Increments per-edge reference counts and updates adjacency when an edge
	///   transitions from zero to one.
	#[inline]
	fn insert(&mut self, signers: &FxHashSet<Address>) {
		// ensure nodes
		for &a in signers {
			self.adj.entry(a).or_default();
		}

		// per-signer RC
		for &a in signers {
			*self.order_rc.entry(a).or_insert(0) += 1;
		}

		// edges + adjacency
		let keys: Vec<Address> = signers.iter().copied().collect();
		for i in 0..keys.len() {
			for j in (i + 1)..keys.len() {
				let (u, v) = if keys[i] <= keys[j] {
					(keys[i], keys[j])
				} else {
					(keys[j], keys[i])
				};
				let rc = self.edge_rc.entry((u, v)).or_insert(0);
				*rc += 1;
				if *rc == 1 {
					self.adj.entry(u).or_default().insert(v);
					self.adj.entry(v).or_default().insert(u);
				}
			}
		}
	}

	#[inline]
	/// Removes a previously inserted order's signer set from the connectivity
	/// graph and returns a `RemovalDelta` summarizing structural changes.
	///
	/// Effects:
	/// - Decrements per-signer reference counts and removes nodes that drop to
	///   zero, along with their incident adjacency.
	/// - Decrements per-edge reference counts and removes edges that drop to zero
	///   from both the edge map and adjacency.
	fn remove(&mut self, signers: &FxHashSet<Address>) -> RemovalDelta {
		let mut delta = RemovalDelta::default();

		// per-signer decrement; drop nodes that hit zero degree.
		for &a in signers {
			if let Some(rc) = self.order_rc.get_mut(&a) {
				*rc = rc.saturating_sub(1);
				if *rc == 0 {
					self.order_rc.remove(&a);
					if let Some(neigh) = self.adj.remove(&a) {
						for n in neigh {
							if let Some(ns) = self.adj.get_mut(&n) {
								ns.remove(&a);
							}
						}
					}
					delta.removed_signers.insert(a);
					delta.any_node_removed = true;
				}
			}
		}

		// edge decrements; remove edges that hit zero.
		let keys: Vec<Address> = signers.iter().copied().collect();
		for i in 0..keys.len() {
			for j in (i + 1)..keys.len() {
				let (u, v) = if keys[i] <= keys[j] {
					(keys[i], keys[j])
				} else {
					(keys[j], keys[i])
				};
				if let Some(rc) = self.edge_rc.get_mut(&(u, v)) {
					*rc = rc.saturating_sub(1);
					if *rc == 0 {
						self.edge_rc.remove(&(u, v));
						if let Some(ns) = self.adj.get_mut(&u) {
							ns.remove(&v);
						}
						if let Some(ns) = self.adj.get_mut(&v) {
							ns.remove(&u);
						}
						delta.any_edge_removed = true;
					}
				}
			}
		}

		delta
	}

	/// Returns component assignment and anchors for the current signer graph.
	///
	/// The return value is `(comp_id, anchors)`, where `comp_id` maps each
	/// signer address to a component index, and `anchors` contains the smallest
	/// address (lexicographic) per component. If `anchors.len() == 1`, the graph
	/// is connected.
	fn components(&self) -> (FxHashMap<Address, usize>, Vec<Address>) {
		let mut nodes: Vec<Address> = self.order_rc.keys().copied().collect();
		nodes.sort_unstable();

		let mut comp_id: FxHashMap<Address, usize> = FxHashMap::default();
		let mut comp_anchors: Vec<Address> = Vec::new();
		let mut unassigned: FxHashSet<Address> = nodes.into_iter().collect();

		while let Some(&seed) = unassigned.iter().next() {
			let cid = comp_anchors.len();
			let mut anchor = seed;
			let mut q: VecDeque<Address> = VecDeque::new();
			q.push_back(seed);
			comp_id.insert(seed, cid);
			unassigned.remove(&seed);

			while let Some(u) = q.pop_front() {
				if u < anchor {
					anchor = u;
				}
				if let Some(neigh) = self.adj.get(&u) {
					for &v in neigh {
						if !self.order_rc.contains_key(&v) {
							continue;
						}
						if unassigned.remove(&v) {
							comp_id.insert(v, cid);
							q.push_back(v);
						}
					}
				}
			}
			comp_anchors.push(anchor);
		}

		(comp_id, comp_anchors)
	}
}

/// Lightweight builder to assemble child groups without extra passes or
/// allocations.
#[derive(Default)]
struct GroupBuilder<P: Platform> {
	order_rc: FxHashMap<Address, u32>,
	edge_rc: FxHashMap<(Address, Address), u32>,
	adj: FxHashMap<Address, FxHashSet<Address>>,
	orders: FxHashMap<OrderHash, PooledOrder<P>>,
	anchor_min: Option<Address>,
}

impl<P: Platform> GroupBuilder<P> {
	/// Returns `true` if no orders have been added to this builder yet.
	#[inline]
	fn is_empty(&self) -> bool {
		self.orders.is_empty()
	}

	/// Adds an order (with its hash) to the builder, updating the internal
	/// connectivity tracking structures (per-signer counts, edge counts,
	/// adjacency) and maintaining the minimal anchor candidate.
	#[inline]
	fn push_with_hash(&mut self, hash: OrderHash, order: PooledOrder<P>) {
		if self.orders.contains_key(&hash) {
			return;
		}
		let signers = order.signers().clone();
		debug_assert!(!signers.is_empty());

		self.orders.insert(hash, order);

		// ensure nodes + anchor candidate
		for &a in &signers {
			self.adj.entry(a).or_default();
			self.anchor_min = Some(match self.anchor_min {
				None => a,
				Some(cur) => a.min(cur),
			});
		}

		// per-address participation
		for &a in &signers {
			*self.order_rc.entry(a).or_insert(0) += 1u32;
		}

		// edges + adjacency
		let keys = signers.iter();
		for (n, a) in keys.clone().enumerate() {
			for b in keys.clone().skip(n + 1) {
				let (from, to) = if *a <= *b { (*a, *b) } else { (*b, *a) };
				let rc = self.edge_rc.entry((from, to)).or_insert(0);
				*rc += 1u32;
				if *rc == 1 {
					self.adj.entry(from).or_default().insert(to);
					self.adj.entry(to).or_default().insert(from);
				}
			}
		}
	}

	#[inline]
	/// Finalizes and returns a `Group` from the accumulated state.
	///
	/// The resulting group's `GroupId` uses the minimal signer seen as the
	/// anchor and the provided `epoch`.
	///
	/// `inherited_committed` and `inherited_consumed` are carried over from the
	/// parent group's scheduler to preserve nonce filtering semantics.
	fn finalize(
		self,
		epoch: u64,
		inherited_committed: &NonceState,
		inherited_consumed: &FxHashMap<Address, Nonce>,
	) -> Group<P> {
		let anchor = self
			.anchor_min
			.expect("child must have at least one signer");
		let id = GroupId(anchor, epoch);
		let orders = self.orders;
		let connectivity = Connectivity {
			order_rc: self.order_rc,
			edge_rc: self.edge_rc,
			adj: self.adj,
		};
		let mut scheduler = NonceScheduler::new();
		scheduler.committed_state = inherited_committed.clone();
		scheduler.consumed_frontier.clone_from(inherited_consumed);
		scheduler.rebuild_from_orders(&orders);

		Group(GroupInner::Active(Arc::new(RwLock::new(ActiveInner {
			id,
			orders,
			connectivity,
			scheduler,
		}))))
	}
}

#[derive(Default)]
struct RemovalDelta {
	removed_signers: FxHashSet<Address>,
	any_node_removed: bool,
	any_edge_removed: bool,
}

#[cfg(test)]
mod tests {
	use {
		super::{
			super::super::{
				order::{Order, PooledOrder},
				tests::make_tx,
			},
			*,
		},
		crate::test_utils::{TestablePlatform, *},
		itertools::Itertools,
	};

	#[rblib_test(Ethereum, Optimism)]
	fn frontier_and_ready_basic<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let tx0 = make_tx::<P>(&s0, 0);
		let tx1 = make_tx::<P>(&s0, 1);
		let po0: PooledOrder<P> = Order::Transaction(tx0).into();
		let po1: PooledOrder<P> = Order::Transaction(tx1).into();

		let mut g = Group::new(po0.clone());
		let _ = g.insert(po1.clone()).expect("insert ok");

		let frontier = g.frontier();
		assert_eq!(frontier.len(), 1);
		assert_eq!(frontier[0], po0.hash());

		// Unknown state: ready equals structural frontier
		let ready = g.ready();
		assert_eq!(ready, frontier);

		// After committing nonce 0, po0 should still be ready; po1 not yet
		let mut ns = NonceState::default();
		ns.update([(s0.address(), 0)].into_iter().collect());
		let _ = g.apply_nonce_update(ns);
		let ready2 = g.ready();
		assert_eq!(ready2, vec![po0.hash()]);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_advances_and_prunes_exclusives<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		// Two mutually exclusive orders: same signer and same nonce
		let txa = make_tx::<P>(&s0, 0);
		let txb = make_tx::<P>(&s0, 0);
		let poa: PooledOrder<P> = Order::Transaction(txa).into();
		let pob: PooledOrder<P> = Order::Transaction(txb).into();

		let mut g = Group::new(poa.clone());
		let _ = g.insert(pob.clone()).expect("insert ok");

		// Both have in-degree zero initially
		assert_eq!(g.frontier().len(), 2);
		assert!(g.ready().contains(&poa.hash()));
		assert!(g.ready().contains(&pob.hash()));

		// Consume poa -> pob must be pruned; group should vanish
		let outcome = g.consume(&poa.hash()).expect("present");
		match outcome {
			ConsumeOutcome {
				consumed,
				pruned,
				group: GroupEffect::Vanished { .. },
			} => {
				assert_eq!(consumed.hash(), poa.hash());
				assert_eq!(pruned.len(), 1);
				assert_eq!(pruned[0].hash(), pob.hash());
			}
			other => panic!("expected vanished, got {other:?}"),
		}
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_split_children_inherit_state<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let sa = FundedAccounts::signer(0);
		let sb = FundedAccounts::signer(1);
		// O1 links A and B at nonce 0 each
		let tx_ab_a0 = make_tx::<P>(&sa, 0);
		let tx_ab_b0 = make_tx::<P>(&sb, 0);
		let o1: PooledOrder<P> = Order::Bundle(
			FlashbotsBundle::<P>::default()
				.with_transaction(tx_ab_a0)
				.with_transaction(tx_ab_b0),
		)
		.into();
		// O2 only A at nonce 1, O3 only B at nonce 1
		let tx_a1 = make_tx::<P>(&sa, 1);
		let tx_b1 = make_tx::<P>(&sb, 1);
		let o2: PooledOrder<P> = Order::Transaction(tx_a1).into();
		let o3: PooledOrder<P> = Order::Transaction(tx_b1).into();

		let mut g = Group::new(o1.clone());
		let _ = g.insert(o2.clone()).expect("insert o2");
		let _ = g.insert(o3.clone()).expect("insert o3");

		// Consume linking order -> group splits into two children (A and B)
		let outcome = g.consume(&o1.hash()).expect("present");
		match outcome {
			ConsumeOutcome {
				consumed,
				pruned,
				group: GroupEffect::Split { children, .. },
			} => {
				assert_eq!(consumed.hash(), o1.hash());
				assert!(pruned.is_empty());
				assert_eq!(children.len(), 2);
				// Each child should be ready with its single order at nonce 1
				for child in children {
					let rh = child.ready();
					assert_eq!(rh.len(), 1);
					let h = rh[0];
					assert!(h == o2.hash() || h == o3.hash());
				}
			}
			_ => panic!("expected split"),
		}
	}

	#[rblib_test(Ethereum, Optimism)]
	fn apply_nonce_update_prunes_permanent<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let tx0 = make_tx::<P>(&s0, 0);
		let tx2 = make_tx::<P>(&s0, 2);
		let po0: PooledOrder<P> = Order::Transaction(tx0).into();
		let po2: PooledOrder<P> = Order::Transaction(tx2).into();

		let mut g = Group::new(po0.clone());
		let _ = g.insert(po2.clone()).expect("insert ok");
		assert_eq!(g.len(), 2);

		// On-chain state moves to 1: nonce 0 order becomes permanently invalid and
		// is removed
		let mut ns = NonceState::default();
		ns.update([(s0.address(), 1)].into_iter().collect());
		let removed = g.apply_nonce_update(ns);
		assert_eq!(removed, 1);
		assert_eq!(g.len(), 1);
		// Remaining order (nonce 2) is not ready yet under committed=1
		assert!(g.ready().is_empty());
	}

	#[rblib_test(Ethereum, Optimism)]
	fn new_group_with_one_signer<P: TestablePlatform>() {
		let tx1 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let order1 = Order::Transaction(tx1);
		let po1: PooledOrder<P> = order1.into();

		let group = Group::new(po1.clone());

		assert_eq!(group.len(), 1);
		assert!(group.contains_order(&po1.hash()));
		assert!(group.contains_signer(&FundedAccounts::signer(0).address()));
		assert_eq!(group.signers().len(), 1);

		assert_eq!(group.id().anchor(), FundedAccounts::signer(0).address());
		assert_eq!(group.id().epoch(), 0);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn new_group_with_many_signers<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		let tx1 = make_tx::<P>(&signers[0], 0);
		let tx2 = make_tx::<P>(&signers[1], 0);
		let tx3 = make_tx::<P>(&signers[2], 0);
		let order = Order::Bundle(
			FlashbotsBundle::default()
				.with_transaction(tx1)
				.with_transaction(tx2)
				.with_transaction(tx3),
		);
		let po: PooledOrder<P> = order.into();
		let group = Group::new(po.clone());

		assert_eq!(group.len(), 1);
		assert!(group.contains_order(&po.hash()));
		assert!(group.contains_signer(&signers[0].address()));
		assert!(group.contains_signer(&signers[1].address()));
		assert!(group.contains_signer(&signers[2].address()));
		assert_eq!(group.signers().len(), 3);

		assert_eq!(group.id().anchor(), signers[0].address());
		assert_eq!(group.id().epoch(), 0);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_duplicate_is_noop<P: TestablePlatform>() {
		let tx = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let po: PooledOrder<P> = Order::Transaction(tx).into();

		let mut group = Group::new(po.clone());
		let id0 = group.id();
		assert_eq!(group.len(), 1);

		// inserting same order returns Ok with unchanged id and length
		let res = group.insert(po.clone());
		assert!(matches!(res, Ok(id) if id == id0));
		assert_eq!(group.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_rejects_disjoint_order<P: TestablePlatform>() {
		let tx_a = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let po_a: PooledOrder<P> = Order::Transaction(tx_a).into();
		let mut group = Group::new(po_a);

		let tx_b = make_tx::<P>(&FundedAccounts::signer(1), 0);
		let po_b: PooledOrder<P> = Order::Transaction(tx_b).into();

		let res = group.insert(po_b.clone());
		match res {
			Ok(_) => panic!("expected Err for disjoint insert"),
			Err(returned) => assert_eq!(returned.hash(), po_b.hash()),
		}

		// group unchanged
		assert_eq!(group.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_overlap_keeps_anchor<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2 (sorted by address)
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		// {s0, s1}
		let tx0a = make_tx::<P>(&signers[0], 0);
		let tx1a = make_tx::<P>(&signers[1], 0);
		let bundle1 = FlashbotsBundle::<P>::default()
			.with_transaction(tx0a)
			.with_transaction(tx1a);
		let po1: PooledOrder<P> = Order::Bundle(bundle1).into();
		let mut group = Group::new(po1);
		let id_before = group.id();

		// {s1, s2} (overlap, no smaller anchor)
		let tx1b = make_tx::<P>(&signers[1], 1);
		let tx2b = make_tx::<P>(&signers[2], 0);
		let bundle2 = FlashbotsBundle::<P>::default()
			.with_transaction(tx1b)
			.with_transaction(tx2b);
		let po2: PooledOrder<P> = Order::Bundle(bundle2).into();
		let res = group.insert(po2);
		assert!(matches!(res, Ok(id) if id == id_before));

		assert_eq!(group.id().anchor(), signers[0].address());
		assert_eq!(group.id().epoch(), id_before.epoch());
		assert_eq!(group.len(), 2);
		assert!(group.contains_signer(&signers[2].address()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_with_smaller_signer_updates_id<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();

		// start with {s1, s2} (anchor s1)
		let tx1a = make_tx::<P>(&signers[1], 0);
		let tx2a = make_tx::<P>(&signers[2], 0);
		let bundle1 = FlashbotsBundle::<P>::default()
			.with_transaction(tx1a)
			.with_transaction(tx2a);
		let po1: PooledOrder<P> = Order::Bundle(bundle1).into();
		let mut group = Group::new(po1);
		let id0 = group.id();
		assert_eq!(id0.anchor(), signers[1].address());

		// insert {s0, s2} → anchor shrinks to s0 and epoch +1
		let tx0b = make_tx::<P>(&signers[0], 0);
		let tx2b = make_tx::<P>(&signers[2], 1);
		let bundle2 = FlashbotsBundle::<P>::default()
			.with_transaction(tx0b)
			.with_transaction(tx2b);
		let po2: PooledOrder<P> = Order::Bundle(bundle2).into();
		let res = group.insert(po2);
		let Ok(id1) = res else {
			panic!("expected Ok");
		};
		assert_eq!(id1.anchor(), signers[0].address());
		assert_eq!(id1.epoch(), id0.epoch() + 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn remove_absent_is_none<P: TestablePlatform>() {
		let tx = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let po: PooledOrder<P> = Order::Transaction(tx.clone()).into();
		let other: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&FundedAccounts::signer(1), 0)).into();

		let mut group = Group::new(po);
		let res = group.remove(&other.hash());
		assert!(res.is_none());
		assert_eq!(group.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn remove_last_order_vanishes<P: TestablePlatform>() {
		let signer = FundedAccounts::signer(0);
		let tx = make_tx::<P>(&signer, 0);
		let po: PooledOrder<P> = Order::Transaction(tx).into();
		let h = po.hash();

		let mut group = Group::new(po);
		let old_id = group.id();
		let res = group.remove(&h).expect("present");
		match res.1 {
			GroupEffect::Vanished {
				old_id: id,
				removed_signers,
			} => {
				assert_eq!(id, old_id);
				assert!(removed_signers.contains(&signer.address()));
			}
			_ => panic!("expected Vanished"),
		}
		assert!(group.is_empty());
		assert_eq!(group.len(), 0);
		assert!(!group.contains_signer(&signer.address()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn remove_non_bridge_still_connected_anchor_unchanged<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1
		let signers = (0..2)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let a0 = signers[0].address();
		let a1 = signers[1].address();

		// o_ab: {s0, s1}
		let o_ab: PooledOrder<P> = {
			let tx0 = make_tx::<P>(&signers[0], 0);
			let tx1 = make_tx::<P>(&signers[1], 0);
			let b = FlashbotsBundle::<P>::default()
				.with_transaction(tx0)
				.with_transaction(tx1);
			Order::Bundle(b).into()
		};
		let mut group = Group::new(o_ab.clone());
		let id0 = group.id();

		// o_b: {s1}
		let o_b: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&signers[1], 1)).into();
		let _ = group.insert(o_b.clone());
		assert_eq!(group.len(), 2);

		let res = group.remove(&o_b.hash()).expect("present");
		match res.1 {
			GroupEffect::StillConnected {
				old_id,
				new_id,
				removed_signers,
			} => {
				assert_eq!(old_id, id0);
				assert_eq!(new_id, id0); // anchor unchanged, epoch unchanged
				assert!(removed_signers.is_empty()); // s1 still present via o_ab
			}
			_ => panic!("expected StillConnected"),
		}
		assert_eq!(group.id(), id0);
		assert!(group.contains_signer(&a0));
		assert!(group.contains_signer(&a1));
		assert!(group.contains_order(&o_ab.hash()));
		assert!(!group.contains_order(&o_b.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn remove_anchor_changes_but_connected<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2
		let signers = (0..3)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let a0 = signers[0].address();
		let a1 = signers[1].address();
		let a2 = signers[2].address();

		// o01: {s0, s1}
		let o01: PooledOrder<P> = {
			let tx0 = make_tx::<P>(&signers[0], 0);
			let tx1 = make_tx::<P>(&signers[1], 0);
			let b = FlashbotsBundle::<P>::default()
				.with_transaction(tx0)
				.with_transaction(tx1);
			Order::Bundle(b).into()
		};
		// o12: {s1, s2}
		let o12: PooledOrder<P> = {
			let tx1b = make_tx::<P>(&signers[1], 1);
			let tx2 = make_tx::<P>(&signers[2], 0);
			let b = FlashbotsBundle::<P>::default()
				.with_transaction(tx1b)
				.with_transaction(tx2);
			Order::Bundle(b).into()
		};

		let mut group = Group::new(o01.clone());
		let _ = group.insert(o12.clone());
		let id0 = group.id();
		assert_eq!(id0.anchor(), a0);

		// remove o01, s0 drops, component {s1,s2} remains → anchor a1, epoch +1
		let res = group.remove(&o01.hash()).expect("present");
		match res.1 {
			GroupEffect::StillConnected {
				old_id,
				new_id,
				removed_signers,
			} => {
				assert_eq!(old_id, id0);
				assert_eq!(new_id.anchor(), a1);
				assert_eq!(new_id.epoch(), id0.epoch() + 1);
				assert!(removed_signers.contains(&a0));
			}
			_ => panic!("expected StillConnected with anchor change"),
		}
		assert!(group.contains_signer(&a1));
		assert!(group.contains_signer(&a2));
		assert!(!group.contains_signer(&a0));
		assert!(group.contains_order(&o12.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn remove_bridge_splits_into_children<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2 < s3
		let signers = (0..4)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let a0 = signers[0].address();
		let a1 = signers[1].address();
		let a2 = signers[2].address();
		let a3 = signers[3].address();

		// o01: {s0, s1}
		let o01: PooledOrder<P> = {
			let tx0 = make_tx::<P>(&signers[0], 0);
			let tx1 = make_tx::<P>(&signers[1], 0);
			let b = FlashbotsBundle::<P>::default()
				.with_transaction(tx0)
				.with_transaction(tx1);
			Order::Bundle(b).into()
		};
		// o12: {s1, s2} (bridge)
		let o12: PooledOrder<P> = {
			let tx1b = make_tx::<P>(&signers[1], 1);
			let tx2 = make_tx::<P>(&signers[2], 0);
			let b = FlashbotsBundle::<P>::default()
				.with_transaction(tx1b)
				.with_transaction(tx2);
			Order::Bundle(b).into()
		};
		// o23: {s2, s3}
		let o23: PooledOrder<P> = {
			let tx2b = make_tx::<P>(&signers[2], 1);
			let tx3 = make_tx::<P>(&signers[3], 0);
			let b = FlashbotsBundle::<P>::default()
				.with_transaction(tx2b)
				.with_transaction(tx3);
			Order::Bundle(b).into()
		};

		let mut group = Group::new(o01.clone());
		let _ = group.insert(o12.clone()); // now signers {s0,s1,s2}
		let _ = group.insert(o23.clone()); // overlap via s2
		let parent_id = group.id();
		assert_eq!(parent_id.anchor(), a0);
		assert_eq!(group.len(), 3);

		let res = group.remove(&o12.hash()).expect("present");
		let GroupEffect::Split {
			old_id,
			parent_signers,
			mut children,
		} = res.1
		else {
			panic!("expected Split");
		};
		assert_eq!(old_id, parent_id);
		// parent had all 4 signers after all inserts
		assert!(parent_signers.contains(&a0));
		assert!(parent_signers.contains(&a1));
		assert!(parent_signers.contains(&a2));
		assert!(parent_signers.contains(&a3));
		assert_eq!(children.len(), 2);
		// parent becomes tombstone
		assert!(group.is_empty());

		// sort children by anchor for deterministic assertions
		children.sort_by_key(|g| g.id().anchor());
		let left = children.remove(0);
		let right = children.remove(0);

		assert_eq!(left.id().anchor(), a0);
		assert_eq!(right.id().anchor(), a2);
		assert_eq!(left.id().epoch(), parent_id.epoch() + 1);
		assert_eq!(right.id().epoch(), parent_id.epoch() + 1);

		// orders moved to respective children
		assert!(left.contains_order(&o01.hash()));
		assert!(!left.contains_order(&o23.hash()));
		assert!(right.contains_order(&o23.hash()));
		assert!(!right.contains_order(&o01.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_delegates_to_remove<P: TestablePlatform>() {
		let tx = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let po: PooledOrder<P> = Order::Transaction(tx).into();
		let h = po.hash();
		let mut group = Group::new(po);
		let res = group.discard(&h).expect("present");
		assert_eq!(res.removed.hash(), h);
		match res.group {
			GroupEffect::Vanished { .. } => {}
			_ => panic!("expected Vanished via discard"),
		}
		assert!(group.is_empty());
	}

	#[rblib_test(Ethereum, Optimism)]
	fn insert_into_tombstone_is_rejected<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let tx0 = make_tx::<P>(&s0, 0);
		let po0: PooledOrder<P> = Order::Transaction(tx0).into();
		let mut g = Group::new(po0);
		// Vanish group
		let _ = g.remove(&g.order_hashes()[0]).expect("present");
		assert!(g.is_empty());

		let tx1 = make_tx::<P>(&s0, 1);
		let po1: PooledOrder<P> = Order::Transaction(tx1).into();
		match g.insert(po1.clone()) {
			Ok(_) => panic!("expected Err when inserting into tombstone"),
			Err(ret) => assert_eq!(ret.hash(), po1.hash()),
		}
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_absent_is_none<P: TestablePlatform>() {
		let tx = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let po: PooledOrder<P> = Order::Transaction(tx).into();
		let other: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&FundedAccounts::signer(1), 0)).into();
		let mut g = Group::new(po);
		assert!(g.discard(&other.hash()).is_none());
		assert_eq!(g.len(), 1);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_keeps_mutually_exclusive_alternative<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let po_a: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 0)).into();
		let po_b: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 0)).into();
		let mut g = Group::new(po_a.clone());
		let _ = g.insert(po_b.clone()).expect("insert ok");
		assert_eq!(g.len(), 2);

		// Discard one; the mutually exclusive other remains
		match g.discard(&po_a.hash()).expect("present").group {
			GroupEffect::StillConnected { .. } => {}
			other => panic!("expected StillConnected, got {other:?}"),
		}
		assert_eq!(g.len(), 1);
		assert!(g.contains_order(&po_b.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_bridge_keeps_connected_then_vanish<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let sa = FundedAccounts::signer(0);
		let sb = FundedAccounts::signer(1);
		let sc = FundedAccounts::signer(2);

		// o_ab bridges A-B
		let o_ab: PooledOrder<P> = {
			let txa = make_tx::<P>(&sa, 0);
			let txb = make_tx::<P>(&sb, 0);
			Order::Bundle(
				FlashbotsBundle::<P>::default()
					.with_transaction(txa)
					.with_transaction(txb),
			)
			.into()
		};
		// o_bc bridges B-C
		let o_bc: PooledOrder<P> = {
			let txb2 = make_tx::<P>(&sb, 1);
			let txc = make_tx::<P>(&sc, 0);
			Order::Bundle(
				FlashbotsBundle::<P>::default()
					.with_transaction(txb2)
					.with_transaction(txc),
			)
			.into()
		};

		let mut g = Group::new(o_ab.clone());
		let _ = g.insert(o_bc.clone()).expect("insert ok");
		let res = g.discard(&o_bc.hash()).expect("present");
		match res.group {
			GroupEffect::StillConnected { .. } => {
				// A-B remains connected via o_ab
				assert!(g.contains_order(&o_ab.hash()));
			}
			_ => panic!(
				"expected StillConnected after discarding bridge that doesn't \
				 disconnect"
			),
		}

		// Now discard the remaining bridge, causing vanish
		let res2 = g.discard(&o_ab.hash()).expect("present");
		match res2.group {
			GroupEffect::Vanished { .. } => {}
			_ => panic!("expected Vanished"),
		}
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_bridge_splits_into_children<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1 < s2 < s3; orders form a chain with a bridge in the middle
		let signers = (0..4)
			.map(FundedAccounts::signer)
			.sorted_by(|a, b| a.address().cmp(&b.address()))
			.collect::<Vec<_>>();
		let a0 = signers[0].address();
		let a2 = signers[2].address();

		// o01: {s0, s1}
		let o01: PooledOrder<P> = {
			let tx0 = make_tx::<P>(&signers[0], 0);
			let tx1 = make_tx::<P>(&signers[1], 0);
			Order::Bundle(
				FlashbotsBundle::<P>::default()
					.with_transaction(tx0)
					.with_transaction(tx1),
			)
			.into()
		};
		// o12: {s1, s2} (bridge)
		let o12: PooledOrder<P> = {
			let tx1b = make_tx::<P>(&signers[1], 1);
			let tx2 = make_tx::<P>(&signers[2], 0);
			Order::Bundle(
				FlashbotsBundle::<P>::default()
					.with_transaction(tx1b)
					.with_transaction(tx2),
			)
			.into()
		};
		// o23: {s2, s3}
		let o23: PooledOrder<P> = {
			let tx2b = make_tx::<P>(&signers[2], 1);
			let tx3 = make_tx::<P>(&signers[3], 0);
			Order::Bundle(
				FlashbotsBundle::<P>::default()
					.with_transaction(tx2b)
					.with_transaction(tx3),
			)
			.into()
		};

		let mut g = Group::new(o01.clone());
		let _ = g.insert(o12.clone()).expect("insert o12");
		let _ = g.insert(o23.clone()).expect("insert o23");
		let parent_id = g.id();

		// Discarding the middle bridge should split the group
		let res = g.discard(&o12.hash()).expect("present");
		let GroupEffect::Split {
			old_id,
			mut children,
			..
		} = res.group
		else {
			panic!("expected Split");
		};
		assert_eq!(old_id, parent_id);
		assert!(g.is_empty()); // parent becomes tombstone

		children.sort_by_key(|c| c.id().anchor());
		let left = children.remove(0);
		let right = children.remove(0);
		assert_eq!(left.id().anchor(), a0);
		assert_eq!(right.id().anchor(), a2);
		assert!(left.contains_order(&o01.hash()));
		assert!(right.contains_order(&o23.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn discard_does_not_advance_nonce_frontier<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let po0: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 0)).into();
		let po1: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 1)).into();
		let mut g = Group::new(po0.clone());
		let _ = g.insert(po1.clone()).expect("insert ok");

		// Set committed nonce to 0 so po0 is ready
		let mut ns = NonceState::default();
		ns.update([(s0.address(), 0)].into_iter().collect());
		let _ = g.apply_nonce_update(ns);
		assert!(g.ready().contains(&po0.hash()));

		// Discard po0 should NOT advance frontier; po1 shouldn't be ready
		let _ = g.discard(&po0.hash()).expect("present");
		assert!(g.ready().is_empty());
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_absent_is_none<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let po0: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 0)).into();
		let mut g = Group::new(po0);
		let other: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 1)).into();
		assert!(g.consume(&other.hash()).is_none());
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_still_connected_no_prune_next<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let po0: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 0)).into();
		let po1: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 1)).into();
		let mut g = Group::new(po0.clone());
		let _ = g.insert(po1.clone()).expect("insert ok");
		let res = g.consume(&po0.hash()).expect("present");
		match res {
			ConsumeOutcome {
				consumed,
				pruned,
				group: GroupEffect::StillConnected { .. },
			} => {
				assert_eq!(consumed.hash(), po0.hash());
				assert!(pruned.is_empty());
			}
			_ => panic!("expected StillConnected"),
		}
		assert!(g.contains_order(&po1.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_anchor_changes_but_connected<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		// s0 < s1
		let s0 = FundedAccounts::signer(0);
		let s1 = FundedAccounts::signer(1);
		// o01 includes both; o1 only s1
		let o01: PooledOrder<P> = {
			let tx0 = make_tx::<P>(&s0, 0);
			let tx1 = make_tx::<P>(&s1, 0);
			Order::Bundle(
				FlashbotsBundle::<P>::default()
					.with_transaction(tx0)
					.with_transaction(tx1),
			)
			.into()
		};
		let o1: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s1, 1)).into();
		let mut g = Group::new(o01.clone());
		let _ = g.insert(o1.clone()).expect("insert ok");
		let id0 = g.id();
		let res = g.consume(&o01.hash()).expect("present");
		match res {
			ConsumeOutcome {
				group: GroupEffect::StillConnected { new_id, .. },
				..
			} => {
				assert_eq!(new_id.anchor(), s1.address());
				let expected_epoch = if id0.anchor() == s1.address() {
					id0.epoch()
				} else {
					id0.epoch() + 1
				};
				assert_eq!(new_id.epoch(), expected_epoch);
			}
			_ => panic!("expected StillConnected with anchor change"),
		}
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_out_of_order_advances_frontier<P: TestablePlatform>() {
		let s0 = FundedAccounts::signer(0);
		let po0: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 0)).into();
		let po1: PooledOrder<P> = Order::Transaction(make_tx::<P>(&s0, 1)).into();
		let mut g = Group::new(po0.clone());
		let _ = g.insert(po1.clone()).expect("insert ok");

		// Consume nonce 1 first; allowed by API
		let _ = g.consume(&po1.hash()).expect("present");
		// committed unknown; effective consumed frontier for s0 should be >= 2, so
		// po0 is not ready now
		assert!(g.ready().is_empty());
		// po0 still exists structurally
		assert!(g.contains_order(&po0.hash()));
	}

	#[rblib_test(Ethereum, Optimism)]
	fn consume_prune_conflict_split_but_outcome_still_connected<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let sa = FundedAccounts::signer(0);
		let sb = FundedAccounts::signer(1);
		// o_link ties A and B at nonce 0
		let o_link: PooledOrder<P> = {
			let txa = make_tx::<P>(&sa, 0);
			let txb = make_tx::<P>(&sb, 0);
			Order::Bundle(
				FlashbotsBundle::<P>::default()
					.with_transaction(txa)
					.with_transaction(txb),
			)
			.into()
		};
		// o_conflict conflicts with o_link on A@0
		let o_conflict: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&sa, 0)).into();
		// o_b_only remains for B
		let o_b_only: PooledOrder<P> =
			Order::Transaction(make_tx::<P>(&sb, 1)).into();

		let mut g = Group::new(o_link.clone());
		let _ = g.insert(o_conflict.clone()).expect("insert ok");
		let _ = g.insert(o_b_only.clone()).expect("insert ok");
		let res = g.consume(&o_conflict.hash()).expect("present");
		match res {
			ConsumeOutcome {
				pruned,
				group: GroupEffect::StillConnected { new_id, .. },
				..
			} => {
				// conflict pruned (o_link)
				assert_eq!(pruned.len(), 1);
				assert_eq!(pruned[0].hash(), o_link.hash());
				// group now anchored at B only
				assert_eq!(new_id.anchor(), sb.address());
				assert!(g.contains_order(&o_b_only.hash()));
			}
			other => panic!("expected StillConnected, got {other:?}"),
		}
	}
}
