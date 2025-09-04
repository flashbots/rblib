use {
	super::*,
	core::{
		pin::Pin,
		task::{Context, Poll},
	},
	derive_more::{Deref, DerefMut},
	futures::Stream,
	reth::transaction_pool::{BestTransactions, ValidPoolTransaction},
};

/// This type implements an active stream of incoming orders. It iterates over
/// all eligible orders that were present in the pool prior to the stream being
/// created as well as orders that were received from the pool while this
/// instance is active.
///
/// If the native transaction pool is not disabled in
/// `Config::disable_native_pool` this stream will also include transaction
/// served by the `BestTransactions` iterator from Reth's transaction pool.
pub struct OrdersStream<P: Platform> {
	pool: OrderPool<P>,
	block: BlockContext<P>,
	sub: broadcast::Receiver<Arc<Order<P>>>,
	native: Option<NativePoolIterator<P>>,
}

impl<P: Platform> OrdersStream<P> {
	pub fn new(pool: &OrderPool<P>, block: &BlockContext<P>) -> Self {
		let native = (!pool.config().disable_native_pool)
			.then(|| {
				pool
					.inner
					.host
					.system_pool()
					.map(|pool| NativePoolIterator::new(pool))
			})
			.flatten();

		Self {
			pool: pool.clone(),
			block: block.clone(),
			sub: pool.inner.fanout.subscribe(),
			native,
		}
	}
}

impl<P: Platform> OrdersStream<P> {
	fn poll_next_native_pool_order(&mut self) -> Option<Order<P>> {
		if !self.pool.config().disable_native_pool
			&& let Some(native) = self.native.as_mut()
		{
			if let Some(tx) = native.next() {
				return Some(Order::Transaction(tx.to_consensus()));
			}
		}

		None
	}
}

impl<P: Platform> Stream for OrdersStream<P> {
	type Item = Order<P>;

	fn poll_next(
		self: Pin<&mut Self>,
		ctx: &mut Context<'_>,
	) -> Poll<Option<Self::Item>> {
		let this = self.get_mut();

		if let Some(order) = this.poll_next_native_pool_order() {
			return Poll::Ready(Some(order));
		}

		ctx.waker().wake_by_ref();
		Poll::Pending
	}
}

#[derive(Deref, DerefMut)]
struct NativePoolIterator<P: Platform>(
	Box<
		dyn BestTransactions<
			Item = Arc<ValidPoolTransaction<types::PooledTransaction<P>>>,
		>,
	>,
);

impl<P: Platform> NativePoolIterator<P> {
	pub fn new(system_pool: &impl traits::PoolBounds<P>) -> Self {
		Self(Box::new(system_pool.best_transactions()))
	}
}

unsafe impl<P: Platform> Send for NativePoolIterator<P> {}
unsafe impl<P: Platform> Sync for NativePoolIterator<P> {}
