use {crate::alloy, alloy::primitives::Address};

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
pub struct GroupId(pub Address, pub u64);

impl GroupId {
	pub const ZERO: Self = Self(Address::ZERO, 0);

	pub const fn anchor(&self) -> Address {
		self.0
	}

	pub const fn epoch(&self) -> u64 {
		self.1
	}
}

/// Merges multiple group ids into a single group id by applying the rules
/// defined in the `GroupId` documentation.
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
