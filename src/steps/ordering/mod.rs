use {
	crate::{
		alloy::primitives::{Address, B256},
		prelude::*,
	},
	core::marker::PhantomData,
	std::{
		collections::{BTreeMap, BTreeSet, HashMap, HashSet},
		fmt::Debug,
		sync::Arc,
	},
};

mod profit;
mod tip;

pub use {profit::OrderByCoinbaseProfit, tip::OrderByPriorityFee};

/// A trait that implements logic for assigning a score to an order.
/// Different implementations of this trait provide different ordering
/// strategies for orders, such as by total profit or by priority fee.
pub trait OrderScore<P: Platform>:
	Clone + Default + Debug + Sync + Send + 'static
{
	type Score: Clone + Ord + Eq + core::hash::Hash;
	type Error: core::error::Error + Send + Sync + 'static;

	fn score(_: &Checkpoint<P>) -> Result<Self::Score, Self::Error>;
}

/// A generic implementation of a step that will order checkpoints based on a
/// scoring function. During the sorting transactions will preserve their
/// sender/nonce dependencies. Transactions within one bundle will remain in
/// the same order to each other, but the order of bundles may change.
///
/// Sorting happens only for the mutable part of the payload, i.e. after
/// the last barrier checkpoint. Anything prior to the last barrier
/// checkpoint is considered immutable and will not be reordered.
#[derive(Debug, Clone, Default)]
pub struct OrderBy<P: Platform, S: OrderScore<P>>(PhantomData<(P, S)>);
impl<P: Platform, S: OrderScore<P>> Step<P> for OrderBy<P, S> {
	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		_ctx: StepContext<P>,
	) -> ControlFlow<P> {
		// create a span that contains all staged checkpoints in the payload.
		// We're guaranteed that all checkpoints in this span are executables,
		// because the staging history begins after the last barrier checkpoint.
		let history = payload.history_staging();

		// Find the correct order of orders in the payload.
		let ordered = match SortedOrders::<P, S>::try_from(&history) {
			Ok(ordered) => ordered.into_iter(),
			// when the step started running, the payload had no nonce conflicts and
			// all orders were able to construct valid checkpoints. After reordering,
			// we should not have any nonce conflicts. This error might happen only
			// when the scoring function fails to compute the score for some
			// checkpoint.
			Err(e) => return e.into(),
		};

		// find the position of the first checkpoint that is not in the correct
		// order.
		let Some(first_out_of_order) = history
			.iter()
			.zip(ordered.clone())
			.position(|(a, b)| a.hash() != b.hash())
		else {
			// all checkpoints in the payload are already in the correct order
			return ControlFlow::Ok(payload);
		};

		// reconstruct the suffix of the payload on top of the correctly ordered
		// prefix.
		let mut ordered_prefix = history
			.at(first_out_of_order)
			.cloned()
			.expect("must be a valid index");

		for item in ordered.skip(first_out_of_order) {
			ordered_prefix = match ordered_prefix.apply(item) {
				Ok(checkpoint) => checkpoint,
				Err(e) => return e.into(),
			};
		}

		assert_eq!(
			ordered_prefix.depth(),
			payload.depth(),
			"payload length should not change after priority fee ordering"
		);

		ControlFlow::Ok(ordered_prefix)
	}
}

/// A structure that sorts orders based on a scoring strategy while preserves
/// nonce dependencies between transactions in the orders. Transactions within a
/// bundle are guaranteed to remain in the same relative order to each other,
/// but the order of bundles may change based on the scoring strategy.
struct SortedOrders<'a, P: Platform, S: OrderScore<P>> {
	/// A map of all signers and the set of orders they have transactions in.
	by_signer: HashMap<Address, HashSet<B256>>,

	/// a map of all orders, sorted by their score.
	by_score: BTreeMap<S::Score, BTreeSet<B256>>,

	/// A map of all signers to the nonces they have used in sorted order.
	nonces: HashMap<Address, BTreeSet<u64>>,

	/// A map of all checkpoints, keyed by their hash.
	all_orders: HashMap<B256, &'a Checkpoint<P>>,

	_scoring: PhantomData<S>,
}

impl<'a, P: Platform, S: OrderScore<P>> TryFrom<&'a Span<P>>
	for SortedOrders<'a, P, S>
{
	type Error = S::Error;

	fn try_from(span: &'a Span<P>) -> Result<Self, Self::Error> {
		let executables = span.iter().filter(|checkpoint| !checkpoint.is_barrier());
		let all_orders: HashMap<B256, &'a Checkpoint<P>> = executables
			.map(|checkpoint| (checkpoint.hash().unwrap(), checkpoint))
			.collect();

		let by_signer: HashMap<Address, HashSet<B256>> = all_orders
			.iter()
			.flat_map(|(hash, checkpoint)| {
				checkpoint
					.transactions()
					.iter()
					.map(move |tx| (tx.signer(), *hash))
			})
			.fold(HashMap::new(), |mut acc, (signer, hash)| {
				acc.entry(signer).or_default().insert(hash);
				acc
			});

		let mut by_score = BTreeMap::<_, BTreeSet<_>>::default();
		for (hash, checkpoint) in &all_orders {
			let score = S::score(checkpoint)?;
			by_score.entry(score).or_default().insert(*hash);
		}

		let nonces: HashMap<Address, BTreeSet<u64>> = all_orders
			.values()
			.flat_map(|order| order.nonces())
			.fold(HashMap::new(), |mut acc, (signer, nonce)| {
				acc.entry(signer).or_default().insert(nonce);
				acc
			});

		Ok(Self {
			by_signer,
			by_score,
			nonces,
			all_orders,
			_scoring: PhantomData,
		})
	}
}

impl<'a, P: Platform, S: OrderScore<P>> IntoIterator
	for SortedOrders<'a, P, S>
{
	type IntoIter = std::vec::IntoIter<Self::Item>;
	type Item = &'a Checkpoint<P>;

	fn into_iter(mut self) -> Self::IntoIter {
		let mut ordered = Vec::with_capacity(self.all_orders.len());

		while let Some(order) = self.pop_best() {
			ordered.push(order);
		}

		ordered.into_iter()
	}
}

impl<'a, P: Platform, S: OrderScore<P>> SortedOrders<'a, P, S> {
	pub(crate) fn pop_best(&mut self) -> Option<&'a Checkpoint<P>> {
		let mut skip = 0;

		let (order, score) = 'order: loop {
			// get the next order with the highest score
			let (score, order_hash) = self.nth_order_by_score(skip)?;
			let order = *self.all_orders.get(&order_hash)?;

			for (signer, nonce) in order.nonces() {
				// if there is another order for the same signer with a lower nonce,
				// we cannot use this order yet before the lower nonce is included.
				if *self.nonces.get(&signer)?.iter().next()? < nonce {
					// skip this order
					skip += 1;
					continue 'order;
				}
			}

			// this is the best order we can use right now
			break (order, score);
		};

		self.remove_order(order, &score);
		Some(order)
	}

	fn remove_order(&mut self, order: &Checkpoint<P>, score: &S::Score) {
		// remove nonces entries
		for (signer, nonce) in order.nonces() {
			if let Some(nonces) = self.nonces.get_mut(&signer) {
				nonces.remove(&nonce);
				if nonces.is_empty() {
					self.nonces.remove(&signer);
				}
			}
		}

		let order_hash = order.hash().expect("order is not barrier");

		// remove by_score entry
		if let Some(orders) = self.by_score.get_mut(score) {
			orders.remove(&order_hash);
			if orders.is_empty() {
				self.by_score.remove(score);
			}
		}

		// remove by_signer entry
		for signer in order.signers() {
			if let Some(orders) = self.by_signer.get_mut(&signer) {
				orders.remove(&order_hash);
				if orders.is_empty() {
					self.by_signer.remove(&signer);
				}
			}
		}

		// remove from all_orders
		self.all_orders.remove(&order_hash);
	}

	fn nth_order_by_score(&self, skip: usize) -> Option<(S::Score, B256)> {
		let mut skip = skip;

		for (score, orders) in self.by_score.iter().rev() {
			if skip < orders.len() {
				let order_hash = orders.iter().nth(skip)?;
				return Some((score.clone(), *order_hash));
			}

			skip = skip.checked_sub(orders.len())?;
		}

		None
	}
}
