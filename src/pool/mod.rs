//! Order pool

use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		consensus::crypto::RecoveryError,
		primitives::{B256, TxHash},
	},
	dashmap::{DashMap, DashSet},
	reth::{
		ethereum::primitives::SignedTransaction,
		primitives::{Recovered, SealedHeader},
	},
	std::sync::Arc,
	tokio::sync::broadcast,
};

mod config;
mod filter;
mod host;
mod insert;
mod maintain;
mod native;
mod order;
mod pipeline;
mod report;
mod rpc;
mod score;
mod setup;
mod stream;

#[cfg(test)]
mod tests;

// Order Pool public API
pub use {
	config::Config,
	filter::*,
	order::Order,
	pipeline::{
		AppendOrders,
		OrderInclusionAttempt,
		OrderInclusionFailure,
		OrderInclusionSuccess,
	},
	rpc::{BundleResult, BundlesApiClient},
	setup::HostNodeInstaller,
};

/// Implements an order pool that handles mempool operations for transactions
/// and bundles.
///
/// Notes:
///
///  - This type is cheap to clone, all clones of this type share the same
///    underlying instance.
///
///  - This type is referenced by steps and RPC modules when constructing a
///    pipeline and Reth node.
pub struct OrderPool<P: Platform> {
	inner: Arc<OrderPoolInner<P>>,
}

impl<P: Platform> OrderPool<P> {
	/// Returns true if the order pool knows for sure that the bundle will never
	/// be eligible for inclusion in any block. This check requires the order pool
	/// to be attached to a host Reth node, otherwise it always returns false.
	pub fn is_permanently_ineligible(&self, bundle: &types::Bundle<P>) -> bool {
		self
			.inner
			.host
			.map_tip_header(|header| bundle.is_permanently_ineligible(header))
			.unwrap_or(false)
	}
}

impl<P: Platform> Clone for OrderPool<P> {
	fn clone(&self) -> Self {
		Self {
			inner: Arc::clone(&self.inner),
		}
	}
}

struct OrderPoolInner<P: Platform> {
	/// Set during `OrderPool` construction.
	config: Config<P>,

	orders: DashMap<B256, Order<P>>,

	/// A map that keeps track of transaction hashes and orders that contain
	/// them.
	txmap: DashMap<TxHash, DashSet<B256>>,

	/// A channel that broadcasts new incoming orders to all active sinks.
	fanout: broadcast::Sender<Arc<Order<P>>>,

	/// The host Reth node that this order pool is attached to.
	/// Attachment is done by the `attach_pool` method during node components
	host: Arc<host::HostNode<P>>,
}

impl<P: Platform> OrderPoolInner<P> {
	pub fn new(config: Config<P>) -> Self {
		Self {
			orders: DashMap::new(),
			txmap: DashMap::new(),
			host: Arc::new(host::HostNode::default()),
			fanout: broadcast::Sender::new(config.inflight_backlog_capacity),
			config,
		}
	}

	pub const fn config(&self) -> &Config<P> {
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

#[cfg(feature = "test-utils")]
pub use native::NativeTransactionPool;
