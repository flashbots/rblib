use super::*;

#[derive(Debug, Clone)]
pub enum Order<P: Platform> {
	/// A single transaction.
	Transaction(Recovered<types::Transaction<P>>),

	/// A bundle of transactions.
	Bundle(types::Bundle<P>),
}

impl<P: Platform> Order<P> {
	pub fn hash(&self) -> B256 {
		match self {
			Order::Transaction(tx) => *tx.tx_hash(),
			Order::Bundle(bundle) => bundle.hash(),
		}
	}

	pub fn transactions(&self) -> &[Recovered<types::Transaction<P>>] {
		match self {
			Order::Bundle(bundle) => bundle.transactions(),
			Order::Transaction(tx) => core::slice::from_ref(tx),
		}
	}

	pub fn try_into_executable(self) -> Result<Executable<P>, RecoveryError> {
		match self {
			Order::Transaction(tx) => tx.try_into_executable(),
			Order::Bundle(bundle) => bundle.try_into_executable(),
		}
	}

	pub const fn is_bundle(&self) -> bool {
		matches!(self, Order::Bundle(_))
	}
}
