use {
	super::*,
	crate::alloy,
	alloy::primitives::Address,
	derive_more::{Deref, DerefMut},
	std::collections::HashMap,
};

pub type Nonce = u64;

/// Represents the current committed nonce state for a subset of signers.
///
/// The mapping is partial, i.e., it may not contain every possible signer, and
/// the absence of a signer implies an unknown nonce state for that signer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deref, DerefMut)]
pub struct NonceState(im::HashMap<Address, Nonce>);

impl NonceState {
	/// Tests whether a given order is eligible for inclustion in a block at the
	/// current nonce state. If any of the signers in the order is not present in
	/// the state, `None` is returned to indicate unknown eligibility.
	pub fn is_eligible<P: Platform>(
		&self,
		order: &Order<P>,
	) -> Option<Eligibility> {
		for (addr, order_nonce) in order.nonces() {
			let state_nonce = *self.get(&addr)?;
			if order_nonce < state_nonce {
				return Some(Eligibility::PermanentlyIneligible);
			} else if order_nonce > state_nonce {
				return Some(Eligibility::TemporarilyIneligible);
			}
		}
		Some(Eligibility::Eligible)
	}

	pub fn update(&mut self, other: NonceState) {
		for (addr, nonce) in other.iter() {
			self.insert(*addr, *nonce);
		}
	}
}

/// This is used to represent the result of dependency check between two orders.
///
/// This relation is idempotent and symmetric in the sense that `between(a, b)`
/// == `between(b, a).inverse()`. The `Independent` and `MutuallyExclusive`
/// cases are self-inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonceRelation {
	/// The two orders are independent, i.e., they do not share any signers and
	/// can be executed in any order.
	Independent,

	/// The two orders cannot both appear in the same valid block. This holds if
	/// any of the following is true:
	/// - They share at least one signer and have an overlapping nonce for that
	///   signer.
	/// - They share signers with disjoint nonce sets, but the nonce dependency
	///   edges between their transactions are not all oriented in the same
	///   direction (i.e. there exists at least one dependency from A→B and one
	///   from B→A, forming a cycle across the two orders).
	///
	/// Intuitively: for two orders to co-exist, every cross-order nonce
	/// dependency must point one-way (all A→B or all B→A). If dependencies exist
	/// in both directions, they are mutually blocking.
	///
	/// Example (cycle):
	///   Order A: `tx_a1` (S1 nonce 1), `tx_a2` (S2 nonce 2)
	///   Order B: `tx_b1` (S1 nonce 2), `tx_b2` (S2 nonce 1)
	/// Here execution would require both directions (A before B for S2; B before
	/// A for S1), so only one order can be included.
	///
	/// Practival examples of orders in this relationship:
	/// - Replacement transactions by same signer with same nonce
	/// - Backruns, they are bundles that are mutually exclusive with the target
	///   tx they are trying to extract MEV from.
	MutuallyExclusive,

	/// Left is a strict prerequisite (ancestor) of right.
	///
	/// Conditions:
	/// - They share at least one signer.
	/// - All cross-order nonce dependency edges are uniformly oriented
	///   left→right.
	/// - For every shared signer, the right order's first nonce equals (last
	///   nonce used by left + 1). No overlaps, no gaps.
	/// - Executable back-to-back in a block: left must precede right.
	///
	/// Reciprocity:
	/// - If left is `DirectAncestor` of right, then right is necessarily
	///   `DirectDescendant` of left.
	DirectAncestor,

	/// Left is a prerequisite (ancestor) of right but with at least one nonce
	/// gap preventing immediate consecutive execution.
	///
	/// Conditions:
	/// - They share at least one signer.
	/// - All cross-order nonce dependency edges are uniformly oriented
	///   left→right.
	/// - For at least one shared signer there is a positive gap between the left
	///   order's final nonce and the right order's first required nonce.
	/// - Additional intervening transaction(s) must fill missing nonce(s) before
	///   the right order becomes executable.
	///
	/// Reciprocity:
	/// - If left is `DistantAncestor` of right, then right is `DistantDescendant`
	///   of left.
	DistantAncestor,

	/// Left is an immediate successor (direct descendant) of right.
	///
	/// Conditions (mirror of DirectAncestor):
	/// - They share at least one signer.
	/// - All cross-order nonce dependency edges are uniformly oriented
	///   right→left.
	/// - For every shared signer, left's first nonce equals (right's last nonce +
	///   1).
	/// - Executable back-to-back: right must precede left.
	///
	/// Reciprocity:
	/// - If left is `DirectDescendant` of right, then right is `DirectAncestor`
	///   of left.
	DirectDescendant,

	/// Left is a successor (descendant) of right but cannot follow immediately
	/// due to at least one nonce gap.
	///
	/// Conditions (mirror of DistantAncestor):
	/// - They share at least one signer.
	/// - All cross-order nonce dependency edges are uniformly oriented
	///   right→left.
	/// - At least one shared signer has a nonce gap between right's final nonce
	///   and left's first required nonce.
	/// - Additional intervening transaction(s) must occupy the missing nonce(s)
	///   before left can execute.
	///
	/// Reciprocity:
	/// - If left is `DistantDescendant` of right, then right is `DistantAncestor`
	///   of left.
	DistantDescendant,
}

impl NonceRelation {
	/// Computes the nonce dependency relation between two orders (left, right).
	///
	/// Semantics (see `NonceRelation` for detailed variant docs):
	/// - If the orders share no signer addresses -> `Independent`.
	/// - If any shared signer reuses (signer, nonce) between the two orders or
	///   the per‑signer nonce intervals overlap -> `MutuallyExclusive`.
	/// - Otherwise, for every shared signer one entire interval must be strictly
	///   before the other. All such interval directions must agree globally:
	///     * All left < right  => ancestor / descendant (direct or distant)
	///     * All right < left  => descendant / ancestor (mirrors)
	///
	/// Mixed directions (at least one left<right and one right<left) introduce
	/// a cycle of required execution ordering => `MutuallyExclusive`.
	///
	/// Direct vs Distant:
	/// - Direct* variants require contiguity for every shared signer (later.min
	///   == earlier.max + 1).
	/// - Distant* variants arise when at least one shared signer has a positive
	///   gap (later.min > earlier.max + 1); missing nonces must be filled by
	///   other txs.
	///
	/// Invariants / Assumptions:
	/// - Within a single order, for a given signer, nonces are guaranteed
	///   elsewhere to be strictly increasing without internal gaps; therefore
	///   each signer’s nonce usage can be summarized by a closed interval [min,
	///   max].
	/// - This operation is symmetric and idempotent: `between(a, b) == between(b,
	///   a).inverse()`. The `Independent` and `MutuallyExclusive` cases are
	///   self-inverse.
	pub fn between<P: Platform>(left: &Order<P>, right: &Order<P>) -> Self {
		struct Entry {
			l_min: Nonce,
			l_max: Nonce,
			r_min: Nonce,
			r_max: Nonce,
			r_seen: bool,
		}

		let mut map: HashMap<Address, Entry> =
			HashMap::with_capacity(left.transactions().len());

		// Collect left order signer intervals.
		for (addr, nonce) in left.nonces() {
			map
				.entry(addr)
				.and_modify(|e| {
					if nonce < e.l_min {
						e.l_min = nonce;
					}
					if nonce > e.l_max {
						e.l_max = nonce;
					}
				})
				.or_insert(Entry {
					l_min: nonce,
					l_max: nonce,
					r_min: Nonce::MAX,
					r_max: Nonce::MIN,
					r_seen: false,
				});
		}

		// Incorporate right order intervals only for shared signers.
		let mut shared_any = false;
		for (addr, nonce) in right.nonces() {
			if let Some(e) = map.get_mut(&addr) {
				shared_any = true;
				if e.r_seen {
					if nonce < e.r_min {
						e.r_min = nonce;
					}
					if nonce > e.r_max {
						e.r_max = nonce;
					}
				} else {
					e.r_min = nonce;
					e.r_max = nonce;
					e.r_seen = true;
				}

				// Overlap (inclusive) => mutually exclusive.
				if e.l_min <= e.r_max && e.r_min <= e.l_max {
					return NonceRelation::MutuallyExclusive;
				}
			}
		}

		if !shared_any {
			return NonceRelation::Independent;
		}

		// Evaluate orientation + gaps.
		let mut saw_l2r = false;
		let mut saw_r2l = false;
		let mut all_gaps_zero = true;

		for e in map.values().filter(|e| e.r_seen) {
			debug_assert!(
				e.l_min > e.r_max || e.r_min > e.l_max,
				"overlap should have exited"
			);
			if e.l_max < e.r_min {
				// left before right
				saw_l2r = true;
				let gap = e.r_min - e.l_max - 1;
				if gap != 0 {
					all_gaps_zero = false;
				}
			} else {
				// right before left
				saw_r2l = true;
				let gap = e.l_min - e.r_max - 1;
				if gap != 0 {
					all_gaps_zero = false;
				}
			}

			if saw_l2r && saw_r2l {
				return NonceRelation::MutuallyExclusive; // mixed orientation => cycle
			}
		}

		debug_assert!(
			saw_l2r ^ saw_r2l,
			"Exactly one orientation expected when not exclusive."
		);

		if saw_l2r {
			if all_gaps_zero {
				NonceRelation::DirectAncestor
			} else {
				NonceRelation::DistantAncestor
			}
		} else if all_gaps_zero {
			NonceRelation::DirectDescendant
		} else {
			NonceRelation::DistantDescendant
		}
	}
}

impl NonceRelation {
	pub fn inverse(self) -> Self {
		match self {
			Self::DirectAncestor => Self::DirectDescendant,
			Self::DirectDescendant => Self::DirectAncestor,
			Self::DistantAncestor => Self::DistantDescendant,
			Self::DistantDescendant => Self::DistantAncestor,
			Self::MutuallyExclusive => Self::MutuallyExclusive,
			Self::Independent => Self::Independent,
		}
	}
}

#[cfg(test)]
mod tests {
	use {
		super::{super::tests::make_tx, *},
		crate::test_utils::*,
	};

	// Independent: distinct signers, single transactions.
	#[rblib_test(Ethereum, Optimism)]
	fn tx_independent<P: TestablePlatform>() {
		let tx1 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(1), 0);
		assert_eq!(
			NonceRelation::between(
				&Order::<P>::Transaction(tx1),
				&Order::<P>::Transaction(tx2)
			),
			NonceRelation::Independent
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn tx_mutually_exclusive<P: TestablePlatform>() {
		let tx1 = make_tx::<P>(&FundedAccounts::signer(1), 1);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(1), 1);
		assert_eq!(
			NonceRelation::between(
				&Order::<P>::Transaction(tx1),
				&Order::<P>::Transaction(tx2)
			),
			NonceRelation::MutuallyExclusive
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn tx_direct_ancestor<P: TestablePlatform>() {
		let tx1 = make_tx::<P>(&FundedAccounts::signer(1), 1);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(1), 2);
		assert_eq!(
			NonceRelation::between(
				&Order::<P>::Transaction(tx1),
				&Order::<P>::Transaction(tx2)
			),
			NonceRelation::DirectAncestor
		);
	}

	#[rblib_test(Ethereum, Optimism)]
	fn tx_distant_ancestor<P: TestablePlatform>() {
		let tx1 = make_tx::<P>(&FundedAccounts::signer(1), 1);
		let tx2 = make_tx::<P>(&FundedAccounts::signer(1), 3);
		assert_eq!(
			NonceRelation::between(
				&Order::<P>::Transaction(tx1),
				&Order::<P>::Transaction(tx2)
			),
			NonceRelation::DistantAncestor
		);
	}

	// Independent: bundle vs transaction, no shared signers.
	#[rblib_test(Ethereum, Optimism)]
	fn independent_bundle_vs_tx<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let tx_a0 = make_tx::<P>(&FundedAccounts::signer(0), 0);
		let bundle_a = FlashbotsBundle::<P>::default().with_transaction(tx_a0);
		let tx_b = make_tx::<P>(&FundedAccounts::signer(1), 0);

		let a = Order::<P>::Bundle(bundle_a);
		let b = Order::<P>::Transaction(tx_b);

		assert_eq!(NonceRelation::between(&a, &b), NonceRelation::Independent);
		assert_eq!(NonceRelation::between(&b, &a), NonceRelation::Independent);
	}

	// MutuallyExclusive: overlapping nonce (same signer, same nonce) tx vs tx.
	#[rblib_test(Ethereum, Optimism)]
	fn mutually_exclusive_overlap_single<P: TestablePlatform>() {
		let signer = FundedAccounts::signer(0);
		let tx_a = make_tx::<P>(&signer, 5);
		let tx_b = make_tx::<P>(&signer, 5);

		let a = Order::<P>::Transaction(tx_a);
		let b = Order::<P>::Transaction(tx_b);

		assert_eq!(
			NonceRelation::between(&a, &b),
			NonceRelation::MutuallyExclusive
		);
		assert_eq!(
			NonceRelation::between(&b, &a),
			NonceRelation::MutuallyExclusive
		);
	}

	// MutuallyExclusive: cycle across two signers (no overlapping nonce).
	// A: s0 nonce 0, s1 nonce 1
	// B: s0 nonce 1, s1 nonce 0
	// For s0 => A before B; for s1 => B before A -> mixed direction => cycle.
	#[rblib_test(Ethereum, Optimism)]
	fn mutually_exclusive_cycle<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let s0 = FundedAccounts::signer(0);
		let s1 = FundedAccounts::signer(1);

		let a_tx0 = make_tx::<P>(&s0, 0);
		let a_tx1 = make_tx::<P>(&s1, 1);
		let b_tx0 = make_tx::<P>(&s0, 1);
		let b_tx1 = make_tx::<P>(&s1, 0);

		let a = Order::<P>::Bundle(
			FlashbotsBundle::default()
				.with_transaction(a_tx0)
				.with_transaction(a_tx1),
		);
		let b = Order::<P>::Bundle(
			FlashbotsBundle::default()
				.with_transaction(b_tx0)
				.with_transaction(b_tx1),
		);

		assert_eq!(
			NonceRelation::between(&a, &b),
			NonceRelation::MutuallyExclusive
		);
		assert_eq!(
			NonceRelation::between(&b, &a),
			NonceRelation::MutuallyExclusive
		);
	}

	// DirectAncestor: left before right, contiguous (single signer).
	// A: nonce 0 ; B: nonce 1
	#[rblib_test(Ethereum, Optimism)]
	fn direct_ancestor_single_signer<P: TestablePlatform>() {
		let signer = FundedAccounts::signer(0);
		let a_tx = make_tx::<P>(&signer, 0);
		let b_tx = make_tx::<P>(&signer, 1);

		let a = Order::<P>::Transaction(a_tx);
		let b = Order::<P>::Transaction(b_tx);

		assert_eq!(
			NonceRelation::between(&a, &b),
			NonceRelation::DirectAncestor
		);
		assert_eq!(
			NonceRelation::between(&b, &a),
			NonceRelation::DirectDescendant
		);
	}

	// DistantAncestor: left before right with a gap.
	// A: nonce 0 ; B: nonce 2 (gap at 1)
	#[rblib_test(Ethereum, Optimism)]
	fn distant_ancestor_single_signer<P: TestablePlatform>() {
		let signer = FundedAccounts::signer(0);
		let a_tx = make_tx::<P>(&signer, 0);
		let b_tx = make_tx::<P>(&signer, 2);

		let a = Order::<P>::Transaction(a_tx);
		let b = Order::<P>::Transaction(b_tx);

		assert_eq!(
			NonceRelation::between(&a, &b),
			NonceRelation::DistantAncestor
		);
		assert_eq!(
			NonceRelation::between(&b, &a),
			NonceRelation::DistantDescendant
		);
	}

	// DirectDescendant: left after right contiguous.
	// Right (in call): nonce 3 ; Left: nonce 4
	#[rblib_test(Ethereum, Optimism)]
	fn direct_descendant_single_signer<P: TestablePlatform>() {
		let signer = FundedAccounts::signer(0);
		let earlier = Order::<P>::Transaction(make_tx::<P>(&signer, 3));
		let later = Order::<P>::Transaction(make_tx::<P>(&signer, 4));

		assert_eq!(
			NonceRelation::between(&later, &earlier),
			NonceRelation::DirectDescendant
		);
		assert_eq!(
			NonceRelation::between(&earlier, &later),
			NonceRelation::DirectAncestor
		);
	}

	// DistantDescendant: left after right with gap.
	// Earlier: nonce 3 ; Later: nonce 5
	#[rblib_test(Ethereum, Optimism)]
	fn distant_descendant_single_signer<P: TestablePlatform>() {
		let signer = FundedAccounts::signer(0);
		let earlier = Order::<P>::Transaction(make_tx::<P>(&signer, 3));
		let later = Order::<P>::Transaction(make_tx::<P>(&signer, 5));

		assert_eq!(
			NonceRelation::between(&later, &earlier),
			NonceRelation::DistantDescendant
		);
		assert_eq!(
			NonceRelation::between(&earlier, &later),
			NonceRelation::DistantAncestor
		);
	}

	// DirectAncestor multi-signer: both signers contiguous in same direction.
	// A: s0 nonce 0, s1 nonce 10
	// B: s0 nonce 1, s1 nonce 11
	#[rblib_test(Ethereum, Optimism)]
	fn direct_ancestor_multi_signer<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let s0 = FundedAccounts::signer(0);
		let s1 = FundedAccounts::signer(1);

		let a = Order::<P>::Bundle(
			FlashbotsBundle::default()
				.with_transaction(make_tx::<P>(&s0, 0))
				.with_transaction(make_tx::<P>(&s1, 10)),
		);
		let b = Order::<P>::Bundle(
			FlashbotsBundle::default()
				.with_transaction(make_tx::<P>(&s0, 1))
				.with_transaction(make_tx::<P>(&s1, 11)),
		);

		assert_eq!(
			NonceRelation::between(&a, &b),
			NonceRelation::DirectAncestor
		);
		assert_eq!(
			NonceRelation::between(&b, &a),
			NonceRelation::DirectDescendant
		);
	}

	// DistantAncestor multi-signer: at least one signer has gap.
	// A: s0 nonce 0, s1 nonce 10
	// B: s0 nonce 2 (gap 1), s1 nonce 11 (contiguous)
	#[rblib_test(Ethereum, Optimism)]
	fn distant_ancestor_multi_signer<
		P: TestablePlatform<Bundle = FlashbotsBundle<P>>,
	>() {
		let s0 = FundedAccounts::signer(0);
		let s1 = FundedAccounts::signer(1);

		let a = Order::<P>::Bundle(
			FlashbotsBundle::default()
				.with_transaction(make_tx::<P>(&s0, 0))
				.with_transaction(make_tx::<P>(&s1, 10)),
		);
		let b = Order::<P>::Bundle(
			FlashbotsBundle::default()
                .with_transaction(make_tx::<P>(&s0, 2))  // gap (1)
                .with_transaction(make_tx::<P>(&s1, 11)), // contiguous
		);

		assert_eq!(
			NonceRelation::between(&a, &b),
			NonceRelation::DistantAncestor
		);
		assert_eq!(
			NonceRelation::between(&b, &a),
			NonceRelation::DistantDescendant
		);
	}
}
