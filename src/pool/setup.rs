//! Order pool setup and configuration functionality.
//!
//! Methods in this module are used to configure the host reth node and other
//! environment components to work with the order pool.

use {
	super::{
		rpc::{
			BundlesApiServer,
			BundlesRpcApi,
			TransactionsApiServer,
			TransactionsRpcApi,
		},
		*,
	},
	core::mem::MaybeUninit,
	reth::{
		node::builder::{
			BuilderContext,
			FullNodeComponents,
			FullNodeTypes,
			NodeTypes,
			components::{ComponentsBuilder, PoolBuilder},
			rpc::RpcContext,
		},
		rpc::api::eth::EthApiTypes,
		transaction_pool::TransactionPool,
	},
};

impl<P: PlatformWithRpcTypes> OrderPool<P> {
	/// Returns `true` if the order pool is attached to a host Reth node.
	/// This is done by the `attach_pool` method during node components setup.
	pub fn is_attached_to_host(&self) -> bool {
		self.inner.host.is_attached()
	}

	/// Installs the order pool RPC endpoints for receiving bundles and other
	/// methods offered by the common reth rpc infra.
	pub fn attach_rpc<Node, EthApi>(
		&self,
		rpc_context: &mut RpcContext<Node, EthApi>,
	) -> eyre::Result<()>
	where
		Node: FullNodeComponents<Types = types::NodeTypes<P>>,
		EthApi: EthApiTypes,
	{
		rpc_context
			.modules
			.add_or_replace_configured(BundlesRpcApi::new(self).into_rpc())?;

		if self.config().disable_native_pool {
			// intercept all RPC transactions
			rpc_context
				.modules
				.add_or_replace_configured(TransactionsRpcApi::new(self).into_rpc())?;
		}

		Ok(())
	}

	/// Attaches the order pool to a pipeline, allowing it to listen for
	/// events emitted by the pipeline. Those events help the order pool
	/// garbage collect orders and offer better order proposals.
	pub fn attach_pipeline(&self, pipeline: &Pipeline<P>) {
		tokio::spawn(self.start_pipeline_events_listener(pipeline));
	}
}

/// In the current implementation of the `OrderPool`, we use the default node
/// transaction pool for handling individual non-bundle transactions. This type
/// allows us to reuse the existing pool builder construction logic and store a
/// reference to the constructed pool in the `OrderPoolInner`. It also attaches
/// the order pool to the host node, allowing it to leverage the host node's
/// functionalities.
///
/// Attaching the order pool to the host node is optional, but greatly enhances
/// the functionality of the order pool. See more in the [`HostNode`]
/// documentation.
///
/// In future iterations, we're expecting to have our own implementation of the
/// system transaction pool.
///
/// usage:
/// ```rust
/// Cli::parsed()
/// 		.run(|builder, cli_args| async move {
///     let pool = OrderPool::<FlashBlocks>::default();
/// 		let opnode = OpNode::new(cli_args);
///     let handle = builder
///         .with_types::<OpNode>()
///         .with_components(opnode.components().attach_pool(&pool))
///         .with_add_ons(opnode.add_ons())
///         .launch()
///         .await?;
///     handle.wait_for_node_exit().await
///     Ok(())
/// 		})
/// 		.unwrap();
/// ```
pub trait HostNodeInstaller<P: PlatformWithRpcTypes> {
	type Node: FullNodeTypes<Types: NodeTypes<Primitives = types::Primitives<P>>>;

	/// The type of the `ComponentsBuilder` with a wrapped pool builder.
	type Output<WrappedPoolB>;

	fn attach_pool(
		self,
		with: &OrderPool<P>,
	) -> Self::Output<
		impl PoolBuilder<
			Self::Node,
			Pool: TransactionPool<Transaction = types::PooledTransaction<P>>,
		> + use<Self, P>,
	>;
}

impl<P: PlatformWithRpcTypes, Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>
	HostNodeInstaller<P>
	for ComponentsBuilder<Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>
where
	PoolB: PoolBuilderBounds<P, Node>,
	Node: FullNodeTypes<Types: NodeTypes<Primitives = types::Primitives<P>>>,
{
	type Node = Node;
	type Output<WrappedPoolB> =
		ComponentsBuilder<Node, WrappedPoolB, PayloadB, NetworkB, ExecB, ConsB>;

	fn attach_pool(
		self,
		to: &OrderPool<P>,
	) -> Self::Output<
		impl PoolBuilder<
			Node,
			Pool: TransactionPool<Transaction = types::PooledTransaction<P>>,
		> + use<P, Node, PoolB, PayloadB, NetworkB, ExecB, ConsB>,
	> {
		let mut current_pool_builder = MaybeUninit::uninit();
		let current_components = self.map_pool(|pool| {
			current_pool_builder.write(pool.clone());
			pool
		});

		current_components.pool(SystemPoolWrapper::new(
			// SAFETY: we have just written to the `MaybeUninit` in `map_pool`.
			unsafe { current_pool_builder.assume_init() },
			to.inner.clone(),
		))
	}
}

/// This type wraps the system transaction pool builder upon instantiation by
/// reth it will store a reference to the system transaction pool as well as
/// attach the reth node to the order pool.
#[must_use]
struct SystemPoolWrapper<P: PlatformWithRpcTypes, Builder> {
	builder: Builder,
	order_pool: Arc<OrderPoolInner<P>>,
}

impl<P: PlatformWithRpcTypes, Builder> SystemPoolWrapper<P, Builder> {
	pub fn new<Node>(builder: Builder, order_pool: Arc<OrderPoolInner<P>>) -> Self
	where
		Builder: PoolBuilderBounds<P, Node>,
		Node: FullNodeTypes<Types: NodeTypes<Primitives = types::Primitives<P>>>,
	{
		Self {
			builder,
			order_pool,
		}
	}
}

impl<P, Builder, Node> PoolBuilder<Node> for SystemPoolWrapper<P, Builder>
where
	P: PlatformWithRpcTypes,
	Builder: PoolBuilderBounds<P, Node>,
	Node: FullNodeTypes<Types: NodeTypes<Primitives = types::Primitives<P>>>,
{
	type Pool = Arc<Builder::Pool>;

	async fn build_pool(
		self,
		ctx: &BuilderContext<Node>,
	) -> eyre::Result<Self::Pool> {
		let system_pool = Arc::new(self.builder.build_pool(ctx).await?);
		let order_pool = Arc::clone(&self.order_pool);
		self
			.order_pool
			.host
			.attach(Arc::clone(&system_pool), order_pool, ctx)?;
		Ok(system_pool)
	}
}

trait PoolBuilderBounds<P, Node>:
	PoolBuilder<
		Node,
		Pool: TransactionPool<Transaction = types::PooledTransaction<P>>,
	> + Clone
	+ Send
	+ Sync
where
	P: Platform,
	Node: FullNodeTypes<Types: NodeTypes<Primitives = types::Primitives<P>>>,
{
}

impl<P: Platform, Node, T> PoolBuilderBounds<P, Node> for T
where
	P: Platform,
	T: PoolBuilder<
			Node,
			Pool: TransactionPool<Transaction = types::PooledTransaction<P>>,
		> + Clone
		+ Send
		+ Sync,
	Node: FullNodeTypes<Types: NodeTypes<Primitives = types::Primitives<P>>>,
{
}
