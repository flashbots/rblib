use super::*;

/// Orders query and retrieval
impl<P: Platform> OrderPool<P> {
	pub fn best_orders_for_block<'a>(
		&'a self,
		block: &'a BlockContext<P>,
		ctx: &'a P::CheckpointContext,
	) -> impl Iterator<Item = Order<P>> + 'a {
		let orders_iter = self.inner.orders.iter().filter_map(|entry| match entry
			.value()
		{
			t @ Order::Transaction(_) => Some(t.clone()),
			b @ Order::Bundle(bundle) => {
				bundle.is_eligible(block, ctx).then(|| b.clone())
			}
		});

		PoolsDemux::new(self.inner.host.system_pool(), orders_iter)
	}
}

/// This type is responsible for demultiplexing orders coming from the system
/// transaction pool and the order pool that supports bundles. It is used in the
/// `AppendOneOrder` step to provide a unified iterator over both sources of
/// orders.
///
/// For now the logic is simple: if the order pool has orders, they are returned
/// first, otherwise the system pool orders are returned. This should be refined
/// in the future.
struct PoolsDemux<'o, P: Platform> {
	system_pool_iter: Option<Box<dyn Iterator<Item = Order<P>>>>,
	order_pool_iter: Box<dyn Iterator<Item = Order<P>> + 'o>,
}

impl<'o, P: Platform> PoolsDemux<'o, P> {
	pub(crate) fn new(
		system_pool: Option<&impl traits::PoolBounds<P>>,
		order_pool: impl Iterator<Item = Order<P>> + 'o,
	) -> Self {
		Self {
			system_pool_iter: system_pool.map(|pool| {
				Box::new(
					pool
						.best_transactions()
						.map(|tx| Order::Transaction(tx.to_consensus())),
				) as Box<dyn Iterator<Item = Order<P>>>
			}),
			order_pool_iter: Box::new(order_pool)
				as Box<dyn Iterator<Item = Order<P>> + 'o>,
		}
	}
}

impl<P: Platform> Iterator for PoolsDemux<'_, P> {
	type Item = Order<P>;

	// todo: refine this logic
	fn next(&mut self) -> Option<Self::Item> {
		// for now precedence is given to the order pool
		if let Some(order) = self.order_pool_iter.next() {
			return Some(order);
		}

		// when the order pool is exhausted we fall back to the system pool
		// if we are attached to a reth node.
		if let Some(system_pool_iter) = self.system_pool_iter.as_mut()
			&& let Some(order) = system_pool_iter.next()
		{
			return Some(order);
		}

		None
	}
}
