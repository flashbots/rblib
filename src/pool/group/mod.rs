use {
	super::*,
	crate::alloy,
	alloy::primitives::Address,
	parking_lot::RwLock,
	rustc_hash::{FxHashMap, FxHashSet},
	std::sync::Arc,
};

mod index;

pub use index::Groups;

/// Uniquely identifies a group of orders that have nonce relationships between
/// them. A group id is generated deterministically using the following rules:
/// - The anchor address is the lexicographically smallest address among all
///   signers of orders in the group.
/// - epoch is a counter that is incremented each time a group is updated.
/// - Groups updates:
///   - New group: when a new order is added that does not share any signers
///     with any existing group, a new group is created with epoch 0 and the
///     smallest signer address as the anchor.
///   - Merge groups: when a new order is added that shares signers with
///     multiple existing groups, those groups are merged into a single group.
///     - `anchor' = min(anchor_1, anchor_2, ..., anchor_n)`
///     - `epoch' = max(epoch_1, epoch_2, ..., epoch_n) + 1`
///   - Split groups: for each child:
///     - `anchor' = min(signer in child)`
///     - `epoch' = parent.epoch + 1`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupId(Address, u64);

impl GroupId {
	pub const ZERO: Self = Self(Address::ZERO, 0);

	pub const fn anchor(&self) -> Address {
		self.0
	}

	pub const fn epoch(&self) -> u64 {
		self.1
	}
}

impl FromIterator<GroupId> for GroupId {
	fn from_iter<T: IntoIterator<Item = GroupId>>(iter: T) -> Self {
		let mut anchor = Address::repeat_byte(u8::MAX);
		let mut epoch = u64::MIN;

		for gid in iter {
			if gid.0 < anchor {
				anchor = gid.0;
			}
			if gid.1 > epoch {
				epoch = gid.1;
			}
		}

		Self(anchor, epoch + 1)
	}
}

/// An order group is defined by transitive signer overlap between orders.
///
/// Notes:
/// - Orders belong to exactly one group whose signer set contains all its
///   signers.
/// - Two signers are in the same group iif there exists a path of orders where
///   consecutive items share at least one signer.
/// - A group with no orders is a tombstone. Tombstones are created when the
///   last order in a group is removed.
#[derive(Debug, Clone)]
pub struct Group<P: Platform>(GroupInner<P>);

impl<P: Platform> Group<P> {
	/// Creates a new group containing exactly one order.
	pub fn one(order: PooledOrder<P>) -> Self {
		core::iter::once(order).collect()
	}

	pub const fn is_active(&self) -> bool {
		matches!(self.0, GroupInner::Active(_))
	}

	pub fn id(&self) -> GroupId {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().id,
			GroupInner::Tombstone(inner) => inner.0,
		}
	}

	/// Return the current signer set (keys of order_rc).
	pub fn signers(&self) -> FxHashSet<Address> {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().conn.signers().collect(),
			GroupInner::Tombstone(_) => FxHashSet::default(),
		}
	}

	/// The number of orders in this group.
	pub fn len(&self) -> usize {
		match &self.0 {
			GroupInner::Active(inner) => inner.read().orders.len(),
			GroupInner::Tombstone(_) => 0,
		}
	}

	/// Return all order hashes currently in this group.
	pub fn order_hashes(&self) -> Vec<OrderHash> {
		match &self.0 {
			GroupInner::Active(inner) => {
				inner.read().orders.keys().copied().collect()
			}
			GroupInner::Tombstone(_) => Vec::new(),
		}
	}

	/// Return all orders currently in this group (cloned).
	pub fn orders(&self) -> Vec<PooledOrder<P>> {
		match &self.0 {
			GroupInner::Active(inner) => {
				inner.read().orders.values().cloned().collect()
			}
			GroupInner::Tombstone(_) => Vec::new(),
		}
	}

	/// Set the epoch while preserving the current anchor.
	pub fn set_epoch(&mut self, epoch: u64) {
		if let GroupInner::Active(inner) = &self.0 {
			let anchor = inner.read().id.anchor();
			inner.write().id = GroupId(anchor, epoch);
		}
	}

	/// Inserts a new order into this group. If the group is a tombstone, this
	/// will panic.
	///
	/// Insertion of an order may change the group id if the new order
	/// introduces a new lexicographically smaller signer than the current
	/// anchor.
	pub fn insert(&mut self, order: PooledOrder<P>) {
		let GroupInner::Active(inner) = &self.0 else {
			unreachable!("cannot insert into tombstone group");
		};

		let mut inner = inner.write();
		let order_hash = order.hash();

		if inner.orders.contains_key(&order_hash) {
			return; // already present
		}

		let signers = order.signers().clone();
		debug_assert!(!signers.is_empty());
		debug_assert!(signers.iter().any(|a| inner.conn.adj.contains_key(a)));

		inner.orders.insert(order_hash, order);

		// update connectivity
		inner.conn.insert_order(&signers);

		// update group id if anchor changed
		let mut new_anchor = inner.id.anchor();
		for &a in &signers {
			if a < new_anchor {
				new_anchor = a;
			}
		}
		if new_anchor != inner.id.anchor() {
			inner.id = GroupId(new_anchor, inner.id.epoch() + 1);
		}
	}

	/// Remove an order by its hash. Updates internal graph and returns the
	/// outcome.
	pub fn remove(&mut self, hash: OrderHash) -> GroupRemove<P> {
		let old_id = self.id();

		let inner_arc = match &self.0 {
			GroupInner::Active(arc) => arc.clone(),
			GroupInner::Tombstone(_) => return GroupRemove::NoopAbsent,
		};
		let mut inner = inner_arc.write();

		// Not present → noop.
		let Some(order) = inner.orders.remove(&hash) else {
			return GroupRemove::NoopAbsent;
		};

		// Apply connectivity update for this removal.
		let removed_order_signers = order.signers();
		let delta = inner.conn.remove_order(removed_order_signers);

		// Vanish if no orders remain.
		if inner.orders.is_empty() {
			self.0 = GroupInner::Tombstone(TombstoneInner(old_id));
			return GroupRemove::Vanished {
				old_id,
				removed_signers: delta.removed_signers,
			};
		}

		// If no structural change, it's still one component with same anchor.
		if !delta.any_node_removed && !delta.any_edge_removed {
			return GroupRemove::StillConnected {
				old_id,
				new_id: inner.id,
				removed_signers: delta.removed_signers,
			};
		}

		// Compute components on signer graph.
		let (comp_id, anchors) = inner.conn.components();

		// Single component after removal (no split).
		if anchors.len() == 1 {
			let new_anchor = anchors[0];
			if new_anchor != inner.id.anchor() {
				inner.id = GroupId(new_anchor, inner.id.epoch() + 1);
			}
			return GroupRemove::StillConnected {
				old_id,
				new_id: inner.id,
				removed_signers: delta.removed_signers,
			};
		}

		// Multiple components: build children without cloning all orders.
		let parent_epoch = inner.id.epoch();
		let mut builders: Vec<ChildBuilder<P>> = (0..anchors.len())
			.map(|_| ChildBuilder::default())
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
		for mut b in builders {
			if b.is_empty() {
				continue;
			}
			let child = b.finalize(parent_epoch + 1);
			children.push(child);
		}

		// Parent becomes a tombstone; Groups will replace it with children.
		self.0 = GroupInner::Tombstone(TombstoneInner(old_id));
		GroupRemove::Split {
			old_id,
			parent_signers: comp_id.keys().copied().collect(),
			children,
		}
	}
}

impl<P: Platform> Default for Group<P> {
	fn default() -> Self {
		Self(GroupInner::Tombstone(TombstoneInner(GroupId::ZERO)))
	}
}

impl<P: Platform> FromIterator<PooledOrder<P>> for Group<P> {
	fn from_iter<T: IntoIterator<Item = PooledOrder<P>>>(iter: T) -> Self {
		let mut orders: FxHashMap<OrderHash, PooledOrder<P>> = FxHashMap::default();
		let mut conn = Connectivity::default();
		let mut anchor_min: Option<Address> = None;

		for order in iter {
			let h = order.hash();
			if orders.contains_key(&h) {
				continue; // dedupe
			}
			let signers = order.signers();
			if signers.is_empty() {
				continue; // skip signer-less orders
			}
			orders.insert(h, order.clone());

			// connectivity + anchor
			conn.insert_order(&signers);
			for &a in signers {
				anchor_min = Some(match anchor_min {
					None => a,
					Some(cur) => a.min(cur),
				});
			}
		}

		match anchor_min {
			None => Group(GroupInner::Tombstone(TombstoneInner(GroupId::ZERO))),
			Some(anchor) => {
				let id = GroupId(anchor, 0);
				Group(GroupInner::Active(Arc::new(RwLock::new(ActiveInner {
					id,
					orders,
					conn,
				}))))
			}
		}
	}
}

/// Lightweight builder to assemble child groups without extra passes or
/// allocations.
#[derive(Default)]
struct ChildBuilder<P: Platform> {
	order_rc: FxHashMap<Address, u32>,
	edge_rc: FxHashMap<(Address, Address), u32>,
	adj: FxHashMap<Address, FxHashSet<Address>>,
	orders: FxHashMap<OrderHash, PooledOrder<P>>,
	anchor_min: Option<Address>,
}

impl<P: Platform> ChildBuilder<P> {
	#[inline]
	fn is_empty(&self) -> bool {
		self.orders.is_empty()
	}

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
	fn finalize(self, epoch: u64) -> Group<P> {
		let anchor = self
			.anchor_min
			.expect("child must have at least one signer");
		let id = GroupId(anchor, epoch);
		Group(GroupInner::Active(Arc::new(RwLock::new(ActiveInner {
			id,
			orders: self.orders,
			conn: Connectivity {
				order_rc: self.order_rc,
				edge_rc: self.edge_rc,
				adj: self.adj,
			},
		}))))
	}
}

#[derive(Debug, Clone)]
enum GroupInner<P: Platform> {
	Active(Arc<RwLock<ActiveInner<P>>>),
	Tombstone(TombstoneInner),
}

#[derive(Debug)]
struct ActiveInner<P: Platform> {
	/// Identity of this group.
	id: GroupId,

	/// All orders currently in this group.
	orders: FxHashMap<OrderHash, PooledOrder<P>>,

	/// Signer connectivity (nodes, edges, adjacency).
	conn: Connectivity,
}

#[derive(Debug, Clone)]
struct TombstoneInner(GroupId);

/// Signer graph connectivity for a group.
#[derive(Debug, Default, Clone)]
struct Connectivity {
	order_rc: FxHashMap<Address, u32>,
	edge_rc: FxHashMap<(Address, Address), u32>,
	adj: FxHashMap<Address, FxHashSet<Address>>,
}

#[derive(Default)]
struct RemovalDelta {
	removed_signers: FxHashSet<Address>,
	any_node_removed: bool,
	any_edge_removed: bool,
}

impl Connectivity {
	#[inline]
	fn signers(&self) -> impl Iterator<Item = Address> + '_ {
		self.order_rc.keys().copied()
	}

	#[inline]
	fn insert_order(&mut self, signers: &FxHashSet<Address>) {
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
	fn remove_order(&mut self, signers: &FxHashSet<Address>) -> RemovalDelta {
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

	/// Returns (comp_id per node, component anchors). If anchors.len()==1, graph
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
			let mut q: std::collections::VecDeque<Address> =
				std::collections::VecDeque::new();
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

/// Result of removing an order from a Group.
pub enum GroupRemove<P: Platform> {
	/// The order wasn't present in this group.
	NoopAbsent,

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
