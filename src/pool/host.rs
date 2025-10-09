use {
	super::*,
	crate::pool::native::NativeTransactionPool,
	futures::StreamExt,
	reth::{
		chainspec::EthChainSpec,
		node::builder::{BuilderContext, FullNodeTypes, NodeTypes},
		providers::{
			BlockReaderIdExt,
			CanonStateSubscriptions,
			StateProviderFactory,
		},
		tasks::shutdown::Shutdown,
		transaction_pool::TransactionPool,
	},
	std::sync::OnceLock,
	tracing::debug,
};

/// This type handles the interaction between the order pool and the host Reth
/// node. Having the order pool attached to the host node gives access to
/// canonical chain updates, gives access to the node database, etc. This
/// enables a number of functionalities, such as:
/// - checking for permanent ineligibility of bundles at the RPC level and
///   rejecting them before they are sent to the order pool.
/// - simulation of bundles against the state of the chain at the RPC level.
/// - garbage collection of orders that have transactions that were included in
///   a committed block.
#[derive(Default)]
pub(super) struct HostNode<P: Platform> {
	instances: OnceLock<Instances<P>>,
}

impl<P: Platform> HostNode<P> {
	/// This method is called from [`HostNodeInstaller::attach_pool`]
	/// This binds the host Reth node to the order pool instance.
	/// This method should be called only once and will fail if the pool is
	/// already attached to a host node.
	pub(super) fn attach<Node, Pool>(
		self: &Arc<Self>,
		system_pool: Arc<Pool>,
		order_pool: Arc<OrderPoolInner<P>>,
		builder_context: &BuilderContext<Node>,
	) -> eyre::Result<()>
	where
		Node: FullNodeTypes<Types: NodeTypes<Primitives = types::Primitives<P>>>,
		Pool: traits::PoolBounds<P> + Send + Sync + 'static,
	{
		let tip_header = builder_context
			.provider()
			.latest_header()?
			.unwrap_or_else(|| {
				SealedHeader::new(
					builder_context.chain_spec().genesis_header().clone(),
					builder_context.chain_spec().genesis_hash(),
				)
			});

		self
			.instances
			.set(Instances {
				system_pool: NativeTransactionPool::new(system_pool),
				tip_header: RwLock::new(tip_header),
				order_pool: order_pool.into(),
				state_provider: Arc::new(builder_context.provider().clone()),
				canonical_sub: Arc::new(builder_context.provider().clone()),
				shutdown: builder_context.task_executor().on_shutdown_signal().clone(),
			})
			.map_err(|_| {
				eyre::eyre!("There is a host already attached to this instance")
			})?;

		debug!(
			"OrderPool attached to host node running chain id {} with genesis {}",
			builder_context.chain_spec().chain_id(),
			builder_context.chain_spec().genesis_hash()
		);

		// spawn the maintenance loop that reacts to reth host node events
		builder_context.task_executor().spawn_critical(
			"OrderPool maintenance loop",
			Arc::clone(self).maintenance_loop(),
		);

		Ok(())
	}

	/// Returns true if the host Reth node is attached to the `OrderPool`.
	/// Attachment is done by the `attach_pool` method during node components
	/// setup.
	pub(super) fn is_attached(&self) -> bool {
		self.instances.get().is_some()
	}

	/// If attached, invokes the provided function with the current tip header.
	pub(super) fn map_tip_header<F, R>(&self, op: F) -> Option<R>
	where
		F: FnOnce(&SealedHeader<types::Header<P>>) -> R,
	{
		self.instances.get().map(|i| op(&*i.tip_header.read()))
	}

	/// If attached to a host node, this will return a reference to the reth
	/// native transaction pool.
	pub(super) fn system_pool(&self) -> Option<&impl traits::PoolBounds<P>> {
		self.instances.get().map(|i| &i.system_pool)
	}

	/// If attached to a host node, this will remove a transaction from the reth
	/// native transaction pool.
	pub(super) fn remove_transaction(&self, tx_hash: TxHash) {
		if let Some(pool) = self.instances.get().map(|i| &i.system_pool) {
			pool.remove_transactions(vec![tx_hash]);
		}
	}
}

impl<P: Platform> HostNode<P> {
	async fn maintenance_loop(self: Arc<Self>) {
		let instances = self.instances.get().expect("HostNode must be attached");
		let mut chain_events = instances.canonical_sub.canonical_state_stream();
		let mut shutdown = instances.shutdown.clone();

		loop {
			tokio::select! {
				// Changes to the canonical chain, reverts, reorgs, commits, etc.
				Some(event) = chain_events.next() => {
					// remove orders that have transactions included in the committed block
					for block in event.committed().blocks().values() {
						instances.order_pool.report_committed_block(block);
					}

					// update the tip header with the latest block header
					*instances.tip_header.write() = event.tip().sealed_header().clone();
				}

				// Reth node shutdown signal. Terminate the maintenance loop.
				() = &mut shutdown => {
					break;
				}
			}
		}
	}
}

#[cfg(feature = "test-utils")]
impl<P: PlatformWithRpcTypes> HostNode<P> {
	pub(crate) fn attach_to_test_node<
		C: crate::test_utils::ConsensusDriver<P>,
	>(
		self: &Arc<Self>,
		node: &crate::test_utils::LocalNode<P, C>,
		order_pool: OrderPool<P>,
	) -> eyre::Result<()> {
		use parking_lot::lock_api::RwLock;

		let tip_header = SealedHeader::new(
			node.config().chain.genesis_header().clone(),
			node.config().chain.genesis_hash(),
		);

		self
			.instances
			.set(Instances {
				order_pool,
				system_pool: node.pool().clone(),
				state_provider: node.state_provider().clone(),
				canonical_sub: node.canonical_chain_updates().clone(),
				tip_header: RwLock::new(tip_header),
				shutdown: node.on_shutdown(),
			})
			.map_err(|_| {
				eyre::eyre!("There is a host already attached to this instance")
			})?;

		node.task_manager().executor().spawn_critical(
			"HostNode maintenance loop",
			Arc::clone(self).maintenance_loop(),
		);
		Ok(())
	}
}

struct Instances<P: Platform> {
	/// The transaction pool constructed during reth node setup.
	/// In this iteration of the `OrderPool` implementation, this is where
	/// individual transactions are handled. Future iterations are expected to
	/// replace this implementation with a more sophisticated one.
	system_pool: NativeTransactionPool<P>,

	/// Access to the chain state provider.
	#[allow(dead_code)]
	state_provider: Arc<dyn StateProviderFactory>,

	/// Access to chain canonical chain updates stream
	canonical_sub:
		Arc<dyn CanonStateSubscriptions<Primitives = types::Primitives<P>>>,

	/// The block header of the last safe block that we have received an update
	/// for.
	tip_header: RwLock<SealedHeader<types::Header<P>>>,

	/// The order pool that owns this instance and is attached to the
	/// host Reth node.
	order_pool: OrderPool<P>,

	/// A future that resolves when the host node is shutting down.
	shutdown: Shutdown,
}

#[cfg(feature = "test-utils")]
impl<P: PlatformWithRpcTypes> OrderPool<P> {
	pub fn attach_to_test_node<C: crate::test_utils::ConsensusDriver<P>>(
		&self,
		node: &crate::test_utils::LocalNode<P, C>,
	) -> eyre::Result<()> {
		let inner = Arc::clone(&self.inner);
		self.inner.host.attach_to_test_node(node, inner.outer())
	}
}
