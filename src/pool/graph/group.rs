use super::*;

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
pub enum OrderGroup<P: Platform> {
	/// This order group contains only one independent order with exactly one
	/// signer. This is a common case for unbundled transactions.
	Single(PooledOrder<P>),

	/// This order group contains orders that form a linear chain of dependencies
	/// without branches.
	///
	/// Nonce dependencies between orders in this variant can only be `Ancestor`
	/// or `Descendant`. We might have nonce gaps between orders, but there are
	/// no cycles, mutually exclusive orders or branches.
	///
	/// If a new order arrives that is mutually exclusive with any order the
	/// group is promoted into a `Graph` variant.
	Linear(im::Vector<PooledOrder<P>>),

	/// This order group contains mutually exclusive orders (such as transactions
	/// and bundles with their backruns, or two txs with the same nonce) that can
	/// branch into several possible outcomes. Nonce dependencies between orders
	/// in this graph can be any of the variants of `NonceRelation`.
	Graph {
		orders: im::HashMap<OrderHash, PooledOrder<P>>,
		edges: im::HashMap<OrderHash, im::Vector<(OrderHash, NonceRelation)>>,
	},
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
	pub fn new() -> Self {
		Self(rand::random())
	}
}

impl<P: Platform> OrderGroup<P> {
	pub fn orders(&self) -> impl Iterator<Item = &PooledOrder<P>> {
		match &self {
			Self::Single(order) => OrdersIter::Single(std::iter::once(order)),
			Self::Linear(orders) => OrdersIter::Linear(orders.iter()),
			Self::Graph { orders, .. } => OrdersIter::Graph(orders.values()),
		}
	}

	/// Returns a list of all unique signers in this order group.
	pub fn signers(&self) -> Vec<Address> {
		match self {
			Self::Single(order) => order.signers().collect(),
			Self::Linear(orders) => {
				orders.iter().flat_map(|o| o.signers()).unique().collect()
			}
			Self::Graph { orders, .. } => orders
				.iter()
				.flat_map(|(_, order)| order.signers())
				.unique()
				.collect(),
		}
	}

	pub fn merge(self, other: Self) -> Option<Self> {
		match (self, other) {
			// single + single = linear | graph
			(Self::Single(o1), Self::Single(o2)) => merge_single_with_single(o1, o2),

			// single + linear = linear | graph
			(Self::Single(o1), Self::Linear(l2))
			| (Self::Linear(l2), Self::Single(o1)) => merge_single_with_linear(o1, l2),

			// linear + linear =
			// - linear if both linears share the same one signer and have no nonce
			//   conflicts
			// - graph otherwise
			(Self::Linear(l1), Self::Linear(l2)) => merge_linear_with_linear(l1, l2),

			// anything + graph = graph
			(o, Self::Graph { orders, edges })
			| (Self::Graph { orders, edges }, o) => {
				todo!()
			}
		}
	}
}

enum OrdersIter<'a, P: Platform> {
	Single(std::iter::Once<&'a PooledOrder<P>>),
	Linear(im::vector::Iter<'a, PooledOrder<P>>),
	Graph(im::hashmap::Values<'a, OrderHash, PooledOrder<P>>),
}

impl<'a, P: Platform> Iterator for OrdersIter<'a, P> {
	type Item = &'a PooledOrder<P>;

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			OrdersIter::Single(i) => i.next(),
			OrdersIter::Linear(i) => i.next(),
			OrdersIter::Graph(i) => i.next(),
		}
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		match self {
			OrdersIter::Single(i) => i.size_hint(),
			OrdersIter::Linear(i) => i.size_hint(),
			OrdersIter::Graph(i) => i.size_hint(),
		}
	}
}

/// Owned iterator for `IntoIterator`.
pub enum OwnedOrdersIter<P: Platform> {
	Single(std::iter::Once<PooledOrder<P>>),
	Linear(std::vec::IntoIter<PooledOrder<P>>),
	Graph(std::vec::IntoIter<PooledOrder<P>>),
}

impl<P: Platform> Iterator for OwnedOrdersIter<P> {
	type Item = PooledOrder<P>;

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			OwnedOrdersIter::Single(i) => i.next(),
			OwnedOrdersIter::Linear(i) | OwnedOrdersIter::Graph(i) => i.next(),
		}
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		match self {
			OwnedOrdersIter::Single(i) => i.size_hint(),
			OwnedOrdersIter::Linear(i) | OwnedOrdersIter::Graph(i) => i.size_hint(),
		}
	}
}

impl<P: Platform> IntoIterator for OrderGroup<P> {
	type IntoIter = OwnedOrdersIter<P>;
	type Item = PooledOrder<P>;

	fn into_iter(self) -> Self::IntoIter {
		match self {
			Self::Single(order) => OwnedOrdersIter::Single(std::iter::once(order)),
			Self::Linear(orders) => {
				// Using Vec to avoid depending on im iterator internals.
				let v: Vec<_> = orders.into_iter().collect();
				OwnedOrdersIter::Linear(v.into_iter())
			}
			Self::Graph { orders, .. } => {
				// Drop edges; we only need the orders for iteration.
				let v: Vec<_> = orders.into_iter().map(|(_, o)| o).collect();
				OwnedOrdersIter::Graph(v.into_iter())
			}
		}
	}
}

/// Merges two orders into an order group if they share at least one signer.
/// The resulting order group will be eigher
/// - Linear if both orders have exactly one signer in common and no nonce
///   conflicts.
/// - Graph otherwise.
fn merge_single_with_single<P: Platform>(
	o1: PooledOrder<P>,
	o2: PooledOrder<P>,
) -> Option<OrderGroup<P>> {
	fn into_graph<P: Platform>(
		o1: PooledOrder<P>,
		o2: PooledOrder<P>,
		o1_o2_rel: NonceRelation,
	) -> OrderGroup<P> {
		let o1_hash = o1.hash();
		let o2_hash = o2.hash();

		OrderGroup::Graph {
			orders: im::hashmap! {
				o1_hash => o1,
				o2_hash => o2,
			},
			edges: im::hashmap! {
				o1_hash => im::vector![(o2_hash, o1_o2_rel)],
				o2_hash => im::vector![(o1_hash, o1_o2_rel.inverse())]
			},
		}
	}

	fn is_linear<P: Platform>(o1: &PooledOrder<P>, o2: &PooledOrder<P>) -> bool {
		o1.transactions().len() == 1
			&& o2.transactions().len() == 1
			&& o1.signers().eq(o2.signers())
	}

	match NonceRelation::between(&o1, &o2) {
		NonceRelation::Independent => None,
		NonceRelation::MutuallyExclusive => {
			Some(into_graph(o1, o2, NonceRelation::MutuallyExclusive))
		}
		rel @ (NonceRelation::DirectAncestor | NonceRelation::DistantAncestor) => {
			if is_linear(&o1, &o2) {
				Some(OrderGroup::Linear(im::vector![o1, o2]))
			} else {
				Some(into_graph(o1, o2, rel))
			}
		}
		rel @ (NonceRelation::DirectDescendant
		| NonceRelation::DistantDescendant) => {
			if is_linear(&o2, &o1) {
				Some(OrderGroup::Linear(im::vector![o2, o1]))
			} else {
				Some(into_graph(o1, o2, rel))
			}
		}
	}
}

fn merge_single_with_linear<P: Platform>(
	single: PooledOrder<P>,
	linear: im::Vector<PooledOrder<P>>,
) -> Option<OrderGroup<P>> {
	None
}

fn merge_linear_with_linear<P: Platform>(
	a: im::Vector<PooledOrder<P>>,
	b: im::Vector<PooledOrder<P>>,
) -> Option<OrderGroup<P>> {
	None
}

fn merge_with_graph<P: Platform>(
	orders: impl Iterator<Item = PooledOrder<P>>,
	graph: OrderGroup<P>,
) -> Option<OrderGroup<P>> {
	todo!()
}
