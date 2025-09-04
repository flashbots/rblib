use super::*;

/// Configures the order pool.
#[derive(Debug)]
pub struct Config<P: Platform> {
	/// When set to true, the `OrderPool` will handle all incoming orders
	/// and individual transactions. For this option to take effect the order
	/// pool must have its rpc modules attached to the reth instance during node
	/// construction using the `extend_rpc_modules` method on the node builder.
	///
	/// ```rust
	/// let pool = OrderPool::default();
	/// builder
	/// 	.with_types::<OpNode>()
	/// 	.with_components(..)
	/// 	.with_add_ons(..)
	/// 	.extend_rpc_modules(move |mut ctx| poo.attach_rpc(&mut ctx))
	/// 	.launch()
	/// ```
	///
	/// When this is set to `false`, then the `OrderPool` is used only for
	/// handling bundles and the native Reth transaction pool handles individual
	/// transactions. For this option to take effect the order pool must attach
	/// itself to the reth components builder during node construction.
	///
	/// ```rust
	/// let pool = OrderPool::default();
	/// builder
	/// 	.with_types::<OpNode>()
	/// 	.with_components(opnode.components.attach_pool(&pool))
	/// 	.with_add_ons(..)
	/// 	.launch()
	/// ```
	///
	/// This default to `false` now, but expect this to change as the `OrderPool`
	/// implementation stabilizes.
	pub disable_native_pool: bool,

	/// The capacity of the broadcast channel that is used to feed currently
	/// active `OrderStream`s. Adjust this value if you notice that the rate of
	/// incoming orders exceeds the rate at which they are consumed by
	/// `OrdersStream`.
	pub inflight_backlog_capacity: usize,

	/// A list of filter predicates that are applied to orders in the pool to
	/// decide whether they should be discarded from the pool or presented by
	/// order streams.
	///
	/// See [`OrderFilter`] for more information on how to implement custom
	/// filters.
	pub filters: Vec<Box<dyn OrderFilter<P>>>,
}

impl<P: Platform> Default for Config<P> {
	fn default() -> Self {
		Self {
			disable_native_pool: false,
			inflight_backlog_capacity: 128,
			filters: vec![
				Box::new(BaseFeeFilter),
				Box::new(NonceFilter),
				Box::new(SignerBalanceFilter),
				Box::new(BundleFilter),
			],
		}
	}
}

impl<P: Platform> Config<P> {
	/// Adds a new filter predicate to the list of filters that are applied to
	/// orders in the pool.
	#[must_use]
	pub fn with_filter(mut self, filter: impl OrderFilter<P>) -> Self {
		self.filters.push(Box::new(filter));
		self
	}
}

impl<P: Platform> Default for OrderPool<P> {
	fn default() -> Self {
		Self::new(Config::default())
	}
}

/// Construction & Configuration
impl<P: Platform> OrderPool<P> {
	/// Creates a new instance of the order pool with the given configuration
	/// value.
	pub fn new(config: Config<P>) -> Self {
		Self {
			inner: Arc::new(OrderPoolInner::new(config)),
		}
	}

	/// Creates a new instance of the order pool with the native reth pool
	/// disabled.
	pub fn without_native_pool() -> Self {
		Self::new(Config {
			disable_native_pool: true,
			..Config::default()
		})
	}

	/// Returns a reference to the configuration used to instantiate this order
	/// pool.
	pub fn config(&self) -> &Config<P> {
		self.inner.config()
	}
}
