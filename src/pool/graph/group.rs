use {
	super::{nonce::NonceRelation, *},
	im::hashmap::ConsumingIter,
	rustc_hash::FxHashSet,
	std::collections::HashSet,
};

/// This structure represents a collection of orders that have nonce
/// dependencies and must be executed in a specific order with relation to each
/// other.
///
/// Common examples of orders that belong to one set are:
/// - Transactions from the same signer.
/// - Transactions from the same signer with replacement transactions.
/// - Bundles with backruns of the target transaction.
/// - A list of user transactions + a bundle that backruns one of them.
/// - An arbitrary combination of the above or bundles that share signers.
/// - etc.
///
/// In theory we could represent all order groups as a graph, but here we
/// optimize for the common cases of single independent orders and linear chains
/// of transactions as they are much cheaper to manage than a full graph
/// structure.
#[derive(Clone, Debug)]
pub struct OrderGroup<P: Platform> {
	/// All orders in this group, indexed by their hash.
	orders: im::HashMap<OrderHash, PooledOrder<P>>,

	/// All nonce dependencies between orders in this group.
	/// Each entry maps an order hash to a list of other orders it has a nonce
	/// dependency with, along with the type of dependency.
	///
	/// Notes:
	/// - `NonceRelation::Independent` is not tracked here.
	rels: im::HashMap<OrderHash, im::HashMap<OrderHash, NonceRelation>>,

	/// The current nonces state of all signers of orders in this group.
	///
	/// When an order from the graph is committed, then its nonce is applied to
	/// this state.
	nonces: NonceState,
}

/// A unique identifier for an order group.
/// This has no meaning outside of the internal implementation of the order
/// graph. Its randomly generated each time a new group is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deref)]
pub struct GroupId(u128);

impl Default for GroupId {
	fn default() -> Self {
		Self(rand::random())
	}
}

impl GroupId {
	/// Generates a new random group ID.
	pub fn random() -> Self {
		Self(rand::random())
	}
}

impl<P: Platform> OrderGroup<P> {
	pub fn new(order: PooledOrder<P>) -> Self {
		Self {
			rels: im::HashMap::new(),
			nonces: NonceState::default(),
			orders: im::HashMap::unit(order.hash(), order),
		}
	}
}

impl<P: Platform> OrderGroup<P> {
	/// Returns the number of orders in this group.
	pub fn len(&self) -> usize {
		self.orders.len()
	}

	/// Returns an iterator over all orders in this group in no particular order.
	pub fn orders(&self) -> impl Iterator<Item = &PooledOrder<P>> {
		self.orders.values()
	}

	/// Returns a list of all unique signers in this order group.
	pub fn signers(&self) -> FxHashSet<Address> {
		self
			.orders
			.values()
			.flat_map(|o| o.signers().clone())
			.collect()
	}

	/// Returns true if the given order shares any signers with any order in
	/// this group.
	pub fn shares_signers(&self, order: &PooledOrder<P>) -> bool {
		!self.signers().is_disjoint(&order.signers())
	}

	/// Returns true if this order group shares any signers with another order
	/// group.
	pub fn overlaps_with(&self, other: &Self) -> bool {
		!self.signers().is_disjoint(&other.signers())
	}
}

/// Mutations
impl<P: Platform> OrderGroup<P> {
	pub fn discard(mut self, order_hash: OrderHash) -> RemovalResult<P> {
		todo!("order group discard");
	}

	/// Returns the next best order from the graph, removing it from the graph.
	pub fn pop(mut self) -> PopResult<P> {
		todo!("pop best order from graph");
	}

	pub fn update_nonces(&mut self, nonces: NonceState) -> RemovalResult<P> {
		todo!("update nonces");
	}

	/// Adds a new order to this group.
	///
	/// If the order is already present, this is a no-op.
	/// If the order shares no signers with any existing order in the group, an
	/// error is returned with the original order group.
	pub fn insert(
		mut self,
		order: PooledOrder<P>,
	) -> Result<Self, (Self, PooledOrder<P>)> {
		if self.orders.contains_key(&order.hash()) {
			return Ok(self); // order already present, noop
		}

		if !self.shares_signers(&order) {
			// order does not belong to this group, reject.
			return Err((self, order));
		}

		let order_hash = order.hash();

		// Identify all nonce relations between the new order and existing orders in
		// the group and update the graph accordingly.
		// todo: optimize and prune.
		for (existing_hash, existing_order) in &self.orders {
			let relation = NonceRelation::between(existing_order, &order);
			if relation != NonceRelation::Independent {
				self
					.rels
					.entry(*existing_hash)
					.or_default()
					.insert(order_hash, relation);
				self
					.rels
					.entry(order_hash)
					.or_default()
					.insert(*existing_hash, relation.inverse());
			}
		}

		self.orders.insert(order_hash, order);

		Ok(self)
	}

	#[expect(clippy::result_large_err)]
	pub fn merge(mut self, other: Self) -> Result<Self, (Self, Self)> {
		if !self.overlaps_with(&other) {
			// groups do not share any signers, cannot merge into one group.
			return Err((self, other));
		}

		for order in other {
			let order_hash = order.hash();

			// Identify all nonce relations between the new order and existing orders
			// in the group and update the graph accordingly.
			// todo: optimize and prune.
			for (existing_hash, existing_order) in &self.orders {
				let relation = NonceRelation::between(existing_order, &order);
				if relation != NonceRelation::Independent {
					self
						.rels
						.entry(*existing_hash)
						.or_default()
						.insert(order_hash, relation);
					self
						.rels
						.entry(order_hash)
						.or_default()
						.insert(*existing_hash, relation.inverse());
				}
			}

			self.orders.insert(order_hash, order);
		}

		Ok(self) // TODO
	}
}

/// Debug API
impl<P: Platform> OrderGroup<P> {
	pub fn dot(
		&self,
		orders: &[PooledOrder<P>],
	) -> Result<String, std::fmt::Error> {
		use std::fmt::Write;

		let mut dot = String::new();
		writeln!(&mut dot, "digraph G {{")?;
		writeln!(&mut dot, "  rankdir=LR;")?;
		writeln!(&mut dot)?;

		// Direct Ancestors
		writeln!(&mut dot, "  subgraph direct_ancestors {{")?;
		writeln!(&mut dot, "    label=\"Direct Ancestors\";")?;
		writeln!(&mut dot, "    edge [weight=5];")?;
		for i in 0..orders.len() {
			let order_hash = orders[i].hash();
			for j in 0..orders.len() {
				if i == j {
					continue;
				}
				let other_hash = orders[j].hash();
				if let Some(relation) = self
					.rels
					.get(&order_hash)
					.and_then(|rels| rels.get(&other_hash))
					&& *relation == NonceRelation::DirectAncestor
				{
					writeln!(&mut dot, "    {i} -> {j};")?;
				}
			}
		}
		writeln!(&mut dot, "  }}")?;

		// Distant Ancestors
		writeln!(&mut dot, "  subgraph distant_ancestors {{")?;
		writeln!(&mut dot, "    label=\"Distant Ancestors\";")?;
		writeln!(&mut dot, "    edge [weight=0.5];")?;
		for (i, order) in orders.iter().enumerate() {
			let order_hash = order.hash();
			for (j, other) in orders.iter().enumerate() {
				if i == j {
					continue;
				}
				let other_hash = other.hash();
				if let Some(relation) = self
					.rels
					.get(&order_hash)
					.and_then(|rels| rels.get(&other_hash))
					&& *relation == NonceRelation::DistantAncestor
				{
					writeln!(&mut dot, "    {i} -> {j} [style=\"dotted\"];")?;
				}
			}
		}
		writeln!(&mut dot, "  }}")?;

		// Mutually Exclusive
		writeln!(&mut dot, "  subgraph mutually_exclusive {{")?;
		writeln!(&mut dot, "    label=\"Mutually Exclusive\";")?;
		writeln!(&mut dot, "    edge [weight=3];")?;
		for i in 0..orders.len() {
			let order_hash = orders[i].hash();
			for j in 0..orders.len() {
				if i == j {
					continue;
				}
				let other_hash = orders[j].hash();
				if let Some(relation) = self
					.rels
					.get(&order_hash)
					.and_then(|rels| rels.get(&other_hash))
					&& *relation == NonceRelation::MutuallyExclusive
					&& i < j
				{
					writeln!(&mut dot, "    {i} -> {j} [color=\"red\", dir=\"both\"];")?;
				}
			}
		}
		writeln!(&mut dot, "  }}")?;

		writeln!(&mut dot, "}}")?;
		Ok(dot)
	}
}

/// Represents the result of removing an order from an order group.
pub struct RemovalResult<P: Platform> {
	/// The list of signers that are not used by part of any resulting group
	/// anymore.
	pub dropped_signers: HashSet<Address>,

	/// The list of orders that were removed from the group as part of this
	/// operation.
	pub dropped_orders: Vec<PooledOrder<P>>,

	/// The resulting order groups after the mutation.
	/// When an order is removed from a group, it may:
	/// - destroy the group if it was the only order in it, in that case the
	///   vector is empty.
	/// - split the group into multiple smaller groups if they become disjoint.
	/// - the group remains a single group, but could have different nonces and
	///   signers.
	pub output_groups: Vec<OrderGroup<P>>,
}

/// Represents the result of popping the next best order from an order group.
pub struct PopResult<P: Platform> {
	/// The order that was popped, if any.
	pub order: Option<PooledOrder<P>>,

	/// Nonce updates as a result of committing to this order.
	pub nonces: NonceState,

	/// The resulting groups after this operation. Popping an order may split the
	/// containing group into multiple smaller groups if the remaining orders
	/// become disjoint, or it may destroy the group if it was the only order in
	/// it.
	pub removal: RemovalResult<P>,
}

impl<P: Platform> IntoIterator for OrderGroup<P> {
	type IntoIter = core::iter::Map<
		ConsumingIter<(OrderHash, PooledOrder<P>)>,
		fn((OrderHash, PooledOrder<P>)) -> PooledOrder<P>,
	>;
	type Item = PooledOrder<P>;

	fn into_iter(self) -> Self::IntoIter {
		self.orders.into_iter().map(|(_, o)| o)
	}
}

#[cfg(test)]
mod tests {
	use {
		super::{super::tests::*, *},
		crate::test_utils::*,
	};

	#[rblib_test(Ethereum, Optimism)]
	fn signers_overlap<P: PlatformWithRpcTypes<Bundle = FlashbotsBundle<P>>>() {
		let group1 = OrderGroup::new(
			make_order::<P>(&[SignerIdAndNonce(0, 1), SignerIdAndNonce(1, 2)]).into(),
		);

		let group2 = OrderGroup::new(
			make_order::<P>(&[SignerIdAndNonce(0, 1), SignerIdAndNonce(2, 3)]).into(),
		);

		let group3 = OrderGroup::new(
			make_order::<P>(&[SignerIdAndNonce(2, 4), SignerIdAndNonce(4, 1)]).into(),
		);

		assert!(group1.overlaps_with(&group2));
		assert!(group2.overlaps_with(&group1));

		assert!(group2.overlaps_with(&group3));
		assert!(group3.overlaps_with(&group2));

		assert!(!group1.overlaps_with(&group3));
		assert!(!group3.overlaps_with(&group1));

		let order1 =
			make_order::<P>(&[SignerIdAndNonce(1, 3), SignerIdAndNonce(4, 2)]).into();

		assert!(group1.shares_signers(&order1));
		assert!(!group2.shares_signers(&order1));
		assert!(group3.shares_signers(&order1));
	}

	#[allow(clippy::too_many_lines)]
	#[rblib_test(Ethereum, Optimism)]
	fn insert_order_into_group_smoke<
		P: PlatformWithRpcTypes<Bundle = FlashbotsBundle<P>>,
	>() {
		let orders: Vec<PooledOrder<P>> = vec![
			make_order::<P>(&[SignerIdAndNonce(0, 1), SignerIdAndNonce(1, 2)]).into(), /* o0 */
			make_order::<P>(&[SignerIdAndNonce(2, 3)]).into(), // o1
			make_order::<P>(&[SignerIdAndNonce(0, 2)]).into(), // o2
			make_order::<P>(&[SignerIdAndNonce(1, 2)]).into(), // o3
			make_order::<P>(&[SignerIdAndNonce(1, 7)]).into(), // o4
			make_order::<P>(&[SignerIdAndNonce(1, 7), SignerIdAndNonce(2, 1)]).into(), /* o5 */
			make_order::<P>(&[SignerIdAndNonce(2, 2), SignerIdAndNonce(3, 1)]).into(), /* o6 */
			make_order::<P>(&[
				SignerIdAndNonce(2, 4),
				SignerIdAndNonce(3, 3),
				SignerIdAndNonce(0, 3),
			])
			.into(), /* o7 */
			make_order::<P>(&[SignerIdAndNonce(2, 5), SignerIdAndNonce(1, 4)]).into(), /* o8 */
			make_order::<P>(&[SignerIdAndNonce(0, 2), SignerIdAndNonce(3, 2)]).into(), /* o9 */
			make_order::<P>(&[SignerIdAndNonce(1, 3), SignerIdAndNonce(3, 2)]).into(), /* o10 */
			make_order::<P>(&[SignerIdAndNonce(1, 5), SignerIdAndNonce(3, 1)]).into(), /* o11 */
		];

		let order_hashes = orders.iter().map(|o| o.hash()).collect::<Vec<_>>();

		macro_rules! assert_rel {
			($group:expr, $a:expr, $b:expr, $rel:expr) => {
				assert_eq!(
					$group
						.rels
						.get(&order_hashes[$a])
						.expect(&format!("missing relation from {} to {}", $a, $b))
						.get(&order_hashes[$b]),
					Some(&$rel)
				);
				assert_eq!(
					$group
						.rels
						.get(&order_hashes[$b])
						.expect(&format!("missing relation from {} to {}", $a, $b))
						.get(&order_hashes[$a]),
					Some(&$rel.inverse())
				);
			};
		}

		macro_rules! assert_rel_count {
			($group:expr, $a:expr, $count:expr) => {
				assert_eq!(
					$group
						.rels
						.get(&order_hashes[$a])
						.expect(&format!("missing relations for {}", $a))
						.len(),
					$count
				);
			};
		}

		let group = OrderGroup::new(orders[0].clone());
		assert_eq!(group.len(), 1);

		// o1, disjoint, reject insertion
		assert!(group.clone().insert(orders[1].clone()).is_err());

		// o2, shares signer 0, not mutually exclusive
		let group = group.insert(orders[2].clone()).expect("should insert");
		assert_eq!(group.len(), 2);
		assert_rel!(group, 0, 2, NonceRelation::DirectAncestor);
		assert_rel_count!(group, 0, 1);
		assert_rel_count!(group, 2, 1);

		// o3, shares signer 1, mutually exclusive
		let group = group.insert(orders[3].clone()).expect("should insert");
		assert_eq!(group.len(), 3);
		assert_rel!(group, 0, 3, NonceRelation::MutuallyExclusive);
		assert_rel_count!(group, 3, 1);

		// o4, shares signer 1 with o0 and o3, distant ancestor, no conflicts
		let group = group.insert(orders[4].clone()).expect("should insert");
		assert_eq!(group.len(), 4);
		assert_rel!(group, 3, 4, NonceRelation::DistantAncestor);
		assert_rel!(group, 0, 4, NonceRelation::DistantAncestor);
		assert_rel_count!(group, 4, 2);

		// o5, introduces a new signer, mutually exclusive with o4
		let group = group.insert(orders[5].clone()).expect("should insert");
		assert_eq!(group.len(), 5);
		assert_rel!(group, 3, 5, NonceRelation::DistantAncestor);
		assert_rel!(group, 4, 5, NonceRelation::MutuallyExclusive);
		assert_rel!(group, 0, 5, NonceRelation::DistantAncestor);
		assert_rel_count!(group, 5, 3);

		// o6, introduces a new signer, no conflicts
		let group = group.insert(orders[6].clone()).expect("should insert");
		assert_eq!(group.len(), 6);
		assert_rel!(group, 5, 6, NonceRelation::DirectAncestor);
		assert_rel_count!(group, 6, 1);

		// o7, shares signers with o0, o2, o5 and o6, no conflicts
		let group = group.insert(orders[7].clone()).expect("should insert");
		assert_eq!(group.len(), 7);
		assert_rel!(group, 0, 7, NonceRelation::DistantAncestor);
		assert_rel!(group, 2, 7, NonceRelation::DirectAncestor);
		assert_rel!(group, 5, 7, NonceRelation::DistantAncestor);
		assert_rel!(group, 6, 7, NonceRelation::DistantAncestor);
		assert_rel_count!(group, 7, 4);

		// o8, shares signers with o1, o4, o5, o6 and o7
		let group = group.insert(orders[8].clone()).expect("should insert");
		assert_eq!(group.len(), 8);
		assert_rel!(group, 0, 8, NonceRelation::DistantAncestor);
		assert_rel!(group, 3, 8, NonceRelation::DistantAncestor);
		assert_rel!(group, 8, 4, NonceRelation::DistantAncestor);
		assert_rel!(group, 5, 8, NonceRelation::MutuallyExclusive);
		assert_rel!(group, 6, 8, NonceRelation::DistantAncestor);
		assert_rel!(group, 7, 8, NonceRelation::DirectAncestor);
		assert_rel_count!(group, 8, 6);

		// o9, shares signers with o0, o2, o6 and o7
		let group = group.insert(orders[9].clone()).expect("should insert");
		assert_eq!(group.len(), 9);
		assert_rel!(group, 0, 9, NonceRelation::DirectAncestor);
		assert_rel!(group, 2, 9, NonceRelation::MutuallyExclusive);
		assert_rel!(group, 6, 9, NonceRelation::DirectAncestor);
		assert_rel!(group, 9, 7, NonceRelation::DirectAncestor);
		assert_rel_count!(group, 9, 4);

		// o10,
		let group = group.insert(orders[10].clone()).expect("should insert");

		// o11,
		let group = group.insert(orders[11].clone()).expect("should insert");

		let dot = group.dot(&orders).unwrap();
		println!("{dot}");
	}
}
