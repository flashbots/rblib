use {
	super::*,
	crate::alloy,
	alloy::primitives::Address,
	parking_lot::RwLock,
	rustc_hash::{FxHashMap, FxHashSet},
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
			GroupInner::Active(inner) => {
				inner.read().order_rc.keys().copied().collect()
			}
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
		debug_assert!(signers.iter().any(|a| inner.adj.contains_key(a)));

		inner.orders.insert(order_hash, order);

		// ensure nodes + compute potential new anchor
		let mut new_anchor = inner.id.anchor();
		for &a in &signers {
			inner.adj.entry(a).or_default();
			if a < new_anchor {
				new_anchor = a;
			}
		}

		// per-address participation
		for &a in &signers {
			*inner.order_rc.entry(a).or_insert(0) += 1u32;
		}

		// populate order graph connectivity
		let keys: Vec<Address> = signers.iter().copied().collect();
		for i in 0..keys.len() {
			for j in (i + 1)..keys.len() {
				let from = keys[i].min(keys[j]);
				let to = keys[i].max(keys[j]);
				*inner.edge_rc.entry((from, to)).or_insert(0) += 1;
				inner.adj.entry(from).or_default().insert(to);
				inner.adj.entry(to).or_default().insert(from);
			}
		}

		// update group id if anchor changed
		if new_anchor != inner.id.anchor() {
			inner.id = GroupId(new_anchor, inner.id.epoch() + 1);
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
		let mut order_rc: FxHashMap<Address, u32> = FxHashMap::default();
		let mut edge_rc: FxHashMap<(Address, Address), u32> = FxHashMap::default();
		let mut adj: FxHashMap<Address, FxHashSet<Address>> = FxHashMap::default();
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

			// ensure nodes + anchor candidate
			for &a in signers {
				adj.entry(a).or_default();
				anchor_min = Some(match anchor_min {
					None => a,
					Some(cur) => a.min(cur),
				});
			}

			// per-address participation
			for &a in signers {
				*order_rc.entry(a).or_insert(0) += 1u32;
			}

			// one undirected edge per unordered pair
			let keys = signers.iter();
			for (n, a) in keys.clone().enumerate() {
				for b in keys.clone().skip(n + 1) {
					let (from, to) = if *a <= *b { (*a, *b) } else { (*b, *a) };
					let rc = edge_rc.entry((from, to)).or_insert(0);
					*rc += 1u32;
					if *rc == 1 {
						adj.entry(from).or_default().insert(to);
						adj.entry(to).or_default().insert(from);
					}
				}
			}
		}

		match anchor_min {
			// No orders with signers → tombstone group
			None => Self::default(),
			Some(anchor) => {
				let id = GroupId(anchor, 0);
				Group(GroupInner::Active(Arc::new(RwLock::new(ActiveInner {
					id,
					order_rc,
					orders,
					edge_rc,
					adj,
				}))))
			}
		}
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

	/// All signers currently in this group.
	/// The usize is a reference count of how many orders this signer appears in.
	order_rc: FxHashMap<Address, u32>,

	/// All orders currently in this group.
	orders: FxHashMap<OrderHash, PooledOrder<P>>,

	/// An edge between two signers exists whenever they both appear in the same
	/// order. When a value hits zero, we need to do connectivity check to see if
	/// the group is still a connected component or if it needs to be split.
	edge_rc: FxHashMap<(Address, Address), u32>,

	/// Adjacency list for each signer. Makes BFS connectivity checks more
	/// practical.
	adj: FxHashMap<Address, FxHashSet<Address>>,
}

#[derive(Debug, Clone)]
struct TombstoneInner(GroupId);
