use {super::*, tokio::sync::broadcast::error::SendError};

impl<P: Platform> OrderPool<P> {
	/// Adds a new order to the order pool
	///
	/// Notes:
	///
	/// - Orders inserted to the `OrderPool` become immediatly available to all
	///   active `OrderStream` instances.
	pub fn insert(&self, order: Order<P>) {
		if let Order::Bundle(ref bundle) = order
			&& self.is_permanently_ineligible(bundle)
		{
			return;
		}

		let order = Arc::new(order);
		if let Err(SendError(order)) = self.inner.fanout.send(order) {
			tracing::warn!(
				order = %order.hash(),
				"Failed to send order to broadcast channel"
			);
		}
	}
}

impl<P: Platform> OrderPool<P> {
	pub fn remove(&self, order_hash: &B256) {
		self.inner.orders.remove(order_hash);
		self.inner.host.remove_transaction(*order_hash);
	}

	/// Removes all orders that contain the a specific transaction hash.
	pub fn remove_any_with(&self, txhash: TxHash) {
		self.inner.host.remove_transaction(txhash);
		if let Some((_, orders)) = self.inner.txmap.remove(&txhash) {
			for order_hash in orders {
				self.remove(&order_hash);
			}
		}
	}
}
