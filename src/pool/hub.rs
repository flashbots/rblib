use {
	super::{order::PooledOrder, *},
	crate::pool::graph::OrderGraph,
	arc_swap::ArcSwapOption,
	canon::CanonicalState,
	tokio::sync::broadcast::{self},
};

pub struct Hub<P: Platform> {
	config: Arc<Config<P>>,

	orders: OrderGraph<P>,

	/// Broadcast channel to notify subscribers of new orders.
	fanout: broadcast::Sender<PooledOrder<P>>,

	/// The latest canonical state source that is used for eligibility
	/// checks.
	latest: ArcSwapOption<CanonicalState<P>>,
}

impl<P: Platform> Hub<P> {
	pub fn new(config: Arc<Config<P>>) -> Self {
		Self {
			orders: OrderGraph::default(),
			latest: ArcSwapOption::empty(),
			fanout: broadcast::Sender::new(config.inflight_backlog_capacity),
			config,
		}
	}

	pub fn subscribe(&self) -> broadcast::Receiver<PooledOrder<P>> {
		self.fanout.subscribe()
	}
}

impl<P: Platform> Hub<P> {
	pub fn insert(&self, order: Order<P>) -> eyre::Result<Eligibility> {
		// First run static checks that do not require chain state
		if !Filters::of(&self.config).intrinsic(&order) {
			return Ok(Eligibility::PermanentlyIneligible);
		}

		// Next, run global checks if we have a canonical state source.
		// If there's no state source, we optimistically accept the order
		// and let it be filtered out later when we have chain state at later
		// stages.

		let chain_state = self.latest.load();
		let Some(chain_state) = chain_state.as_ref() else {
			// Can't run eligibility checks without chain state, so we
			// optimistically accept the order for now as ready.
			todo!("insert ready order");
		};

		let eligibility = match Filters::of(&self.config).global(
			&chain_state.state,
			&chain_state.header,
			&order,
		)? {
			e @ (Eligibility::Eligible | Eligibility::TemporarilyIneligible) => e,
			Eligibility::PermanentlyIneligible => {
				return Ok(Eligibility::PermanentlyIneligible);
			}
		};

		Ok(eligibility)
	}

	pub fn discard(&self, order_hash: OrderHash) {
		todo!("discard order {order_hash}");
	}
}
