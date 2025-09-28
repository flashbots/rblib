use {
	super::id::GroupId,
	crate::{
		alloy,
		pool::{OrderHash, PooledOrder},
		prelude::*,
	},
	alloy::primitives::Address,
	parking_lot::RwLock,
	rustc_hash::{FxHashMap, FxHashSet},
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

		Self(GroupInner::Active(Arc::new(RwLock::new(ActiveInner {
			id,
			orders,
			connectivity,
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

		Ok(inner.id)
	}

	/// Removes an order without advancing nonce frontiers.
	///
	/// Use this when the order leaves the pool without inclusion in a payload.
	/// - Keeps mutually exclusive alternatives (they remain eligible).
	/// - Removes descendants that become unsatisfied because they depended on
	///   this order.
	/// - No-op if the order is not in this group.
	pub fn discard(&mut self, order: &OrderHash) -> Option<RemoveOutcome<P>> {
		self.remove(order)
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
	pub fn consume(&mut self, order: &OrderHash) {
		todo!()
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
	/// Remove an order by its hash. Updates internal graph and returns the
	/// outcome.
	pub fn remove(&mut self, hash: &OrderHash) -> Option<RemoveOutcome<P>> {
		self.remove_with_order(hash).map(|(_, out)| out)
	}

	/// Remove an order by its hash and return the removed order and outcome.
	/// This avoids cloning and enables callers to observe the removed payload.
	pub fn remove_with_order(
		&mut self,
		hash: &OrderHash,
	) -> Option<(PooledOrder<P>, RemoveOutcome<P>)> {
		let old_id = self.id();

		let inner_arc = match &self.0 {
			GroupInner::Active(arc) => arc.clone(),
			GroupInner::Tombstone(_) => return None,
		};
		let mut inner = inner_arc.write();

		// Not present → noop.
		let order = inner.orders.remove(hash)?;

		// Apply connectivity update for this removal.
		let removed_order_signers = order.signers();
		let delta = inner.connectivity.remove(removed_order_signers);

		// Vanish if no orders remain.
		if inner.orders.is_empty() {
			self.0 = GroupInner::Tombstone(old_id);
			return Some((order, RemoveOutcome::Vanished {
				old_id,
				removed_signers: delta.removed_signers,
			}));
		}

		// If no structural change, it's still one component with same anchor.
		if !delta.any_node_removed && !delta.any_edge_removed {
			return Some((order, RemoveOutcome::StillConnected {
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
			return Some((order, RemoveOutcome::StillConnected {
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

		let mut children: Vec<Group<P>> = Vec::with_capacity(builders.len());
		for b in builders {
			if b.is_empty() {
				continue;
			}
			let child = b.finalize(parent_epoch + 1);
			children.push(child);
		}

		// Parent becomes a tombstone; Groups will replace it with children.
		self.0 = GroupInner::Tombstone(old_id);
		Some((order, RemoveOutcome::Split {
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
		}

		self_inner.id
	}
}

/// Result of removing an order from a Group.
pub enum RemoveOutcome<P: Platform> {
	/// Group remains a single connected component.
	/// - If anchor didn't change, `new_id` == `old_id`.
	/// - `removed_signers` are addresses that dropped to zero participation.
	StillConnected {
		old_id: GroupId,
		new_id: GroupId,
		removed_signers: FxHashSet<Address>,
	},

	/// Group split into multiple children (each child epoch = parent.epoch + 1).
	/// `parent_signers` is the full set of signers previously indexed for this
	/// group.
	Split {
		old_id: GroupId,
		parent_signers: FxHashSet<Address>,
		children: Vec<Group<P>>,
	},

	/// Group became empty (vanished). `removed_signers` are addresses that
	/// dropped out.
	Vanished {
		old_id: GroupId,
		removed_signers: FxHashSet<Address>,
	},
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
	fn finalize(self, epoch: u64) -> Group<P> {
		let anchor = self
			.anchor_min
			.expect("child must have at least one signer");
		let id = GroupId(anchor, epoch);
		Group(GroupInner::Active(Arc::new(RwLock::new(ActiveInner {
			id,
			orders: self.orders,
			connectivity: Connectivity {
				order_rc: self.order_rc,
				edge_rc: self.edge_rc,
				adj: self.adj,
			},
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
		match res {
			RemoveOutcome::Vanished {
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
		match res {
			RemoveOutcome::StillConnected {
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
		match res {
			RemoveOutcome::StillConnected {
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
		let RemoveOutcome::Split {
			old_id,
			parent_signers,
			mut children,
		} = res
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
		match res {
			RemoveOutcome::Vanished { .. } => {}
			_ => panic!("expected Vanished via discard"),
		}
		assert!(group.is_empty());
	}
}
