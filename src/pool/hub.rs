use {
	super::{order::PooledOrder, *},
	arc_swap::ArcSwapOption,
	canon::CanonicalState,
	dashmap::DashSet,
	tokio::sync::broadcast::{self},
};

pub struct Hub<P: Platform> {
	config: Arc<Config<P>>,
	limbo: Limbo<P>,
	ready: Ready<P>,
	fanout: broadcast::Sender<PooledOrder<P>>,
	latest: ArcSwapOption<CanonicalState<P>>,
}

impl<P: Platform> Hub<P> {
	pub fn new(config: Arc<Config<P>>) -> Self {
		let fanout = broadcast::Sender::new(config.inflight_backlog_capacity);
		Self {
			config,
			fanout,
			limbo: Limbo::default(),
			ready: Ready::default(),
			latest: ArcSwapOption::empty(),
		}
	}

	pub fn subscribe(&self) -> broadcast::Receiver<PooledOrder<P>> {
		self.fanout.subscribe()
	}

	pub const fn ready(&self) -> &Ready<P> {
		&self.ready
	}

	pub const fn limbo(&self) -> &Limbo<P> {
		&self.limbo
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
			self.ready.insert(order.into());
			return Ok(Eligibility::Eligible);
		};

		match Filters::of(&self.config).global(
			&chain_state.state,
			&chain_state.header,
			&order,
		)? {
			Eligibility::Eligible => self.ready.insert(order.into()),
			Eligibility::TemporarilyIneligible => self.limbo.insert(order.into()),
			Eligibility::PermanentlyIneligible => {
				return Ok(Eligibility::PermanentlyIneligible);
			}
		}

		Ok(Eligibility::Eligible)
	}

	pub fn discard(&self, order_hash: OrderHash) {
		todo!("discard order {order_hash}");
	}
}

/// Holds temporarily ineligible orders that might become eligible later.
#[derive(Debug, Default)]
pub struct Limbo<P: Platform> {
	orders: Vec<PooledOrder<P>>,
}

impl<P: Platform> Limbo<P> {
	pub fn insert(&self, _order: PooledOrder<P>) {}
}

#[derive(Debug, Default)]
pub struct Ready<P: Platform> {
	orders: DashSet<PooledOrder<P>>,
}

impl<P: Platform> Ready<P> {
	pub fn insert(&self, order: PooledOrder<P>) {
		self.orders.insert(order);
	}

	pub fn snapshot(&self) -> Vec<PooledOrder<P>> {
		self.orders.iter().map(|o| o.clone()).collect()
	}
}
