use super::*;

/// Extension trait for [`types::Bundle`] that improves Dev Ex when
/// working with platform-agnostic bundles.
pub trait BundleExt<P: Platform> {
	/// Returns an iterator over all transactions that are allowed to fail while
	/// keeping the bundle valid.
	fn failable_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>>;

	/// Returns an iterator over all transactions that can be removed from the
	/// bundle without affecting the bundle validity.
	fn optional_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>>;

	/// Returns an iterator over all transactions that are required to be present
	/// for the bundle to be valid. This includes also transactions that may be
	/// allowed to fail but must be present.
	fn required_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>>;

	/// Returns an iterator over all transactions that must be included and may
	/// not fail for the bundle to be valid.
	fn critical_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>>;
}

impl<P: Platform, T: Bundle<P>> BundleExt<P> for T {
	/// Returns an iterator over all transactions that are allowed to fail while
	/// keeping the bundle valid.
	fn failable_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self
			.transactions()
			.iter()
			.filter(|tx| !self.is_allowed_to_fail(tx.tx_hash()))
	}

	/// Returns an iterator over all transactions that can be removed from the
	/// bundle without affecting the bundle validity.
	fn optional_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self
			.transactions()
			.iter()
			.filter(|tx| self.is_optional(tx.tx_hash()))
	}

	/// Returns an iterator over all transactions that are required to be present
	/// for the bundle to be valid. This includes also transactions that may be
	/// allowed to fail but must be present.
	fn required_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self
			.transactions()
			.iter()
			.filter(|tx| !self.is_optional(tx.tx_hash()))
	}

	/// Returns an iterator over all transactions that must be included and may
	/// not fail for the bundle to be valid.
	fn critical_txs(
		&self,
	) -> impl Iterator<Item = &Recovered<types::Transaction<P>>> {
		self.transactions().iter().filter(|tx| {
			!self.is_allowed_to_fail(tx.tx_hash()) && !self.is_optional(tx.tx_hash())
		})
	}
}
