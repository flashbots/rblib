use {
	super::{graph::OrderGraph, *},
	core::{
		pin::Pin,
		task::{Context, Poll, Waker},
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
	graph: Option<OrderGraph<P>>,
	receiver: BroadcastStream<PooledOrder<P>>,
	wakers: Vec<Waker>,
}

impl<P: Platform> OrdersStream<P> {
	pub fn new(pool: &OrderPool<P>, block: &BlockContext<P>) -> Self {
		tracing::info!(">--> new OrdersStream at block {}", block.number());
		Self {
			block: block.clone(),
			receiver: pool.hub().subscribe().into(),
			graph: Some(pool.hub().graph()),
			wakers: Vec::new(),
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
		let graph = this.graph.take().expect("graph is always Some; qed");

		// make sure that we're up to date with any new orders that arrive after we
		// created the stream.
		if let Poll::Ready(Some(Ok(order))) = this.receiver.poll_next_unpin(cx) {
			this.graph = Some(graph.insert(order));
		}

		// pop the next eligible order from the graph.
		let graph = this.graph.take().expect("graph is always Some; qed");
		if let Some((graph, order)) = graph.pop() {
			this.graph = Some(graph);
			return Poll::Ready(Some(order));
		}

		// if there are no more orders in the graph, we need to wait for new ones to
		// arrive. We register the current task's waker so that we can be woken up
		// when new orders arrive.
		this.wakers.push(cx.waker().clone());

		Poll::Pending
	}
}
