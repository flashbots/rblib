use {crate::prelude::*, hub::Hub, std::sync::Arc};

mod canon;
mod config;
mod deps;
mod filter;
mod hub;
mod order;
mod pipeline;
mod report;
mod rpc;
mod score;
mod stream;

#[cfg(test)]
mod tests;

// Order Pool public API
pub use {
	canon::CanonicalStateSource,
	config::Config,
	filter::*,
	order::{Order, OrderHash, PooledOrder},
	pipeline::{
		AppendOrders,
		OrderInclusionAttempt,
		OrderInclusionFailure,
		OrderInclusionSuccess,
	},
	rpc::{BundleResult, BundlesApiClient},
};

/// Implements an order pool that handles mempool operations for transactions
/// and bundles and their public RPC interfaces.
///
/// Notes:
///
///  - This type is cheap to clone, all clones of this type share the same
///    underlying instance.
///
///  - This type is referenced by steps and RPC modules when constructing a
///    pipeline and Reth node.
///
///  - Attaching the `OrderPool` to a Reth instance is done like this:
/// 	 ```rust
/// 	  let pool = OrderPool::default();
/// 		builder
/// 			.with_types::<OpNode>()
/// 			.with_components(..)
/// 			.with_add_ons(..)
/// 			.extend_rpc_modules(move |mut ctx| pool.attach_reth(&mut ctx))
/// 			.launch()
/// 		```
pub struct OrderPool<P: Platform> {
	inner: Arc<OrderPoolInner<P>>,
}

/// Public API
impl<P: Platform> OrderPool<P> {
	/// Adds a new order to the order pool.
	///
	/// First the order is sent to the `Hub`, which then distributes it to all
	/// active `OrderStream`s and adds it to its internal pending list of orders.
	pub fn insert(&self, order: Order<P>) -> eyre::Result<Eligibility> {
		self.inner.hub.insert(order)
	}
}

impl<P: Platform> Clone for OrderPool<P> {
	fn clone(&self) -> Self {
		Self {
			inner: Arc::clone(&self.inner),
		}
	}
}

/// Internal API
impl<P: Platform> OrderPool<P> {
	fn hub(&self) -> &Hub<P> {
		&self.inner.hub
	}
}

struct OrderPoolInner<P: Platform> {
	/// Set during `OrderPool` construction.
	config: Arc<Config<P>>,

	/// This is the central hub for all incoming orders. They first make their
	/// way to the hub, which then distributes them to the appropriate sinks.
	hub: Hub<P>,

	/// Responsible for keeping the `OrderPool` up to date with the latest
	/// canonical state of the chain.
	canonical: Option<CanonicalStateSource<P>>,
}

impl<P: Platform> OrderPoolInner<P> {
	pub fn new(config: Config<P>, canonical: CanonicalStateSource<P>) -> Self {
		let config = Arc::new(config);
		let hub = Hub::new(Arc::clone(&config));

		Self {
			config,
			hub,
			canonical: Some(canonical),
		}
	}

	pub fn with_config(config: Config<P>) -> Self {
		let config = Arc::new(config);
		let hub = Hub::new(Arc::clone(&config));

		Self {
			config,
			hub,
			canonical: None,
		}
	}

	pub fn with_state_source(canonical: CanonicalStateSource<P>) -> Self {
		let config = Arc::new(Config::default());
		let hub = Hub::new(Arc::clone(&config));

		Self {
			config,
			hub,
			canonical: Some(canonical),
		}
	}

	pub fn config(&self) -> &Config<P> {
		&self.config
	}
}

impl<P: Platform> OrderPoolInner<P> {
	pub fn outer(self: &Arc<Self>) -> OrderPool<P> {
		OrderPool {
			inner: Arc::clone(self),
		}
	}
}

impl<P: Platform> From<Arc<OrderPoolInner<P>>> for OrderPool<P> {
	fn from(inner: Arc<OrderPoolInner<P>>) -> Self {
		inner.outer()
	}
}
