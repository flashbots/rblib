use {
	super::{order::PooledOrder, *},
	crate::pool::graph::OrderGraph,
	arc_swap::{ArcSwap, ArcSwapOption},
	canon::CanonicalState,
	tokio::sync::broadcast,
};

pub struct Hub<P: Platform> {
	config: Arc<Config<P>>,

	graph: ArcSwap<OrderGraph<P>>,

	/// Broadcast channel to notify subscribers of new orders.
	fanout: broadcast::Sender<PooledOrder<P>>,

	/// The latest canonical state source that is used for eligibility
	/// checks.
	latest: ArcSwapOption<CanonicalState<P>>,
}

impl<P: Platform> Hub<P> {
	pub fn new(config: Arc<Config<P>>) -> Self {
		Self {
			latest: ArcSwapOption::empty(),
			graph: ArcSwap::from_pointee(OrderGraph::default()),
			fanout: broadcast::Sender::new(config.inflight_backlog_capacity),
			config,
		}
	}

	pub fn subscribe(&self) -> broadcast::Receiver<PooledOrder<P>> {
		self.fanout.subscribe()
	}

	pub fn graph(&self) -> OrderGraph<P> {
		self.graph.load().as_ref().clone()
	}
}

impl<P: Platform> Hub<P> {
	pub fn insert(&self, order: Order<P>) -> bool {
		// First run static checks that do not require chain state to filter out
		// obviously invalid orders that will never be eligible for inclusion in any
		// current or furute block.
		if !Filters::of(&self.config).intrinsic(&order) {
			return false;
		}

		let order = PooledOrder::from(order);

		// Then broadcast it to all active order streams that are
		// in the process of yielding orders for inclusion in a payload job (if
		// any).
		let _ = self.fanout.send(order.clone());

		// Finally insert it into the order graph maintained by the hub.
		// self
		// 	.graph
		// 	.rcu(|graph| graph.as_ref().clone().insert(order.clone()));

		true
	}

	pub fn discard(&self, order_hash: OrderHash) {
		todo!("discard order {order_hash}");
	}
}
