use super::*;

/// This type is responsible for demultiplexing orders coming from the system
/// transaction pool and the order pool that supports bundles. It is used in the
/// `AppendOneOrder` step to provide a unified iterator over both sources of
/// orders.
///
/// For now the logic is simple: if the order pool has orders, they are returned
/// first, otherwise the system pool orders are returned. This should be refined
/// in the future.
pub struct PoolsDemux<'o, P: Platform> {
	system_pool_iter: Option<Box<dyn Iterator<Item = Order<P>>>>,
	order_pool_iter: Option<Box<dyn Iterator<Item = Order<P>> + 'o>>,
}

impl<'o, P: Platform> PoolsDemux<'o, P> {
	pub fn new<S: traits::PoolBounds<P>>(
		system_pool: Option<&S>,
		order_pool: Option<&'o OrderPool<P>>,
		block: &'o BlockContext<P>,
	) -> Self {
		Self {
			system_pool_iter: system_pool.map(|pool| {
				Box::new(
					pool
						.best_transactions()
						.map(|tx| Order::Transaction(tx.to_consensus())),
				) as Box<dyn Iterator<Item = Order<P>>>
			}),
			order_pool_iter: order_pool.map(|pool| {
				Box::new(pool.best_orders_for_block(block))
					as Box<dyn Iterator<Item = Order<P>> + 'o>
			}),
		}
	}
}

impl<P: Platform> Iterator for PoolsDemux<'_, P> {
	type Item = Order<P>;

	// todo: refine this logic
	fn next(&mut self) -> Option<Self::Item> {
		// for now precedence is given to the local order pool
		if let Some(order_pool_iter) = self.order_pool_iter.as_mut()
			&& let Some(order) = order_pool_iter.next()
		{
			return Some(order);
		}

		if let Some(system_pool_iter) = self.system_pool_iter.as_mut()
			&& let Some(order) = system_pool_iter.next()
		{
			return Some(order);
		}

		None
	}
}
