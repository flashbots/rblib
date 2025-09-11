use super::*;

/// Configures the order pool.
#[derive(Debug)]
pub struct Config<P: Platform> {
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

	/// A list of weighted scoring functions that are used to score orders in the
	/// pool.
	pub scores: Vec<(u32, Box<dyn OrderScore<P>>)>,
}

impl<P: Platform> Default for Config<P> {
	fn default() -> Self {
		Self {
			inflight_backlog_capacity: 128,
			filters: vec![
				Box::new(BasicFilter),
				Box::new(BaseFeeFilter),
				Box::new(NonceFilter),
				Box::new(SignerBalanceFilter),
				Box::new(BundleFilter),
			],
			scores: vec![(1, Box::new(PriorityFeeScore))],
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
			inner: Arc::new(OrderPoolInner::with_config(config)),
		}
	}

	/// Returns a reference to the configuration used to instantiate this order
	/// pool.
	pub fn config(&self) -> &Config<P> {
		self.inner.config()
	}
}
