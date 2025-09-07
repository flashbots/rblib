use {
	super::*,
	core::{
		pin::Pin,
		task::{Context, Poll},
	},
	futures::{Stream, StreamExt},
	tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError},
	tracing::debug,
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
	receiver: BroadcastStream<PooledOrder<P>>,
	pending: Vec<PooledOrder<P>>,
}

impl<P: Platform> OrdersStream<P> {
	pub fn new(pool: &OrderPool<P>, block: &BlockContext<P>) -> Self {
		tracing::info!(">--> new OrdersStream at block {}", block.number());
		Self {
			block: block.clone(),
			receiver: pool.hub().subscribe().into(),
			pending: pool.hub().ready().snapshot(),
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
		if let Some(order) = this.pending.pop() {
			return Poll::Ready(Some(order));
		}

		match this.receiver.poll_next_unpin(cx) {
			Poll::Ready(Some(Ok(order))) => Poll::Ready(Some(order)),
			Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(n)))) => {
				debug!(skipped = n, "Sink lagged behind, skipped some orders");
				cx.waker().wake_by_ref(); // poll again
				Poll::Pending
			}
			Poll::Ready(None) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
	}
}
