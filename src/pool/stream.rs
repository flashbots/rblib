use {
	super::{graph::OrderGraph, *},
	core::{
		pin::Pin,
		task::{Context, Poll},
	},
	futures::{Stream, StreamExt},
	tokio_stream::wrappers::BroadcastStream,
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
	block: BlockContext<P>,
	graph: OrderGraph<P>,
	receiver: BroadcastStream<PooledOrder<P>>,
}

impl<P: Platform> OrdersStream<P> {
	pub fn new(pool: &OrderPool<P>, block: &BlockContext<P>) -> Self {
		tracing::info!(">--> new OrdersStream at block {}", block.number());
		Self {
			block: block.clone(),
			receiver: pool.hub().subscribe().into(),
			graph: pool.hub().graph_snapshot(),
		}
	}
}

impl<P: Platform> Stream for OrdersStream<P> {
	type Item = PooledOrder<P>;

	fn poll_next(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
	) -> Poll<Option<Self::Item>> {
		let this = self.get_mut();

		// make sure that we're up to date with any new orders that arrive after we
		// created the stream.
		if let Poll::Ready(Some(Ok(order))) = this.receiver.poll_next_unpin(cx) {
			this.graph.insert(order);
		}

		if let Some(order) = this.graph.pop_best() {
			cx.waker().wake_by_ref(); // there might be more orders available
			return Poll::Ready(Some(order));
		}
		Poll::Pending
	}
}
