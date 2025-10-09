use {
	super::*,
	crate::{alloy, pool::NativeTransactionPool, prelude::*, reth},
	alloy::{
		consensus::{BlockHeader, SignableTransaction},
		eips::{BlockNumberOrTag, Encodable2718},
		network::{BlockResponse, TransactionBuilder, TxSignerSync},
		providers::*,
		signers::Signature,
	},
	core::{
		any::Any,
		pin::Pin,
		task::{Context, Poll},
		time::Duration,
	},
	futures::FutureExt,
	jsonrpsee_core::client::Client as RcpClient,
	nanoid::nanoid,
	reth::{
		args::{DatadirArgs, NetworkArgs, RpcServerArgs},
		chainspec::EthChainSpec,
		core::exit::NodeExitFuture,
		ethereum::provider::db::{
			ClientVersion,
			DatabaseEnv,
			init_db,
			mdbx::{
				DatabaseArguments,
				KILOBYTE,
				MEGABYTE,
				MaxReadTransactionDuration,
			},
			test_utils::{ERROR_DB_CREATION, TempDatabase},
		},
		node::builder::{rpc::RethRpcAddOns, *},
		payload::builder::PayloadId,
		providers::{CanonStateSubscriptions, StateProviderFactory},
		tasks::{TaskExecutor, TaskManager, shutdown::Shutdown},
	},
	reth_ipc::client::{IpcClientBuilder, IpcError},
	std::{
		sync::Arc,
		time::{SystemTime, UNIX_EPOCH},
	},
	tokio::sync::oneshot,
};

/// Types implementing this trait emulate a consensus node and are responsible
/// for generating `ForkChoiceUpdate`, `GetPayload`, `SetPayload` and other
/// Engine API calls to trigger payload building and canonical chain updates on
/// test [`LocalNode`].
pub trait ConsensusDriver<P: PlatformWithRpcTypes>:
	Sized + Unpin + Send + Sync + 'static
{
	type Params: Default;

	/// Starts the process of building a new block on the node.
	///
	/// This should run the engine api CL <-> EL protocol up to the point of
	/// scheduling the payload build process and receiving a payload ID.
	fn start_building(
		&self,
		node: &LocalNode<P, Self>,
		target_timestamp: u64,
		params: &Self::Params,
	) -> impl Future<Output = eyre::Result<PayloadId>>;

	/// This is always called after a payload building process has successfully
	/// started and a payload ID has been returned.
	///
	/// This should run the engine api CL <-> EL protocol to retrieve the newly
	/// built payload then set it as the canonical payload on the node.
	fn finish_building(
		&self,
		node: &LocalNode<P, Self>,
		payload_id: PayloadId,
		params: &Self::Params,
	) -> impl Future<Output = eyre::Result<types::BlockResponse<P>>>;
}

/// This is used to create local execution nodes for testing purposes that can
/// be used to test payload building pipelines. This type contains everything to
/// trigger full payload building lifecycle and interact with the node through
/// RPC.
pub struct LocalNode<P, C>
where
	P: PlatformWithRpcTypes,
	C: ConsensusDriver<P>,
{
	/// The consensus driver used to build blocks and interact with the node.
	consensus: C,
	/// The exit future that can be used to wait for the node to exit. In
	/// practice this never completes, because the node is kept alive until the
	/// test is done.
	exit_future: NodeExitFuture,
	/// The configuration of the node.
	config: NodeConfig<types::ChainSpec<P>>,
	/// The provider used to interact with the node over rpc.
	provider: RootProvider<types::RpcTypes<P>>,
	/// The provider with access to node local storage
	state_provider: Arc<dyn StateProviderFactory>,
	/// Access to chain canonical chain updates stream
	canon_updates:
		Arc<dyn CanonStateSubscriptions<Primitives = types::Primitives<P>>>,
	/// The task manager used to manage async tasks in the node.
	tasks: Option<TaskManager>,
	/// The transaction pool used by the node.
	pool: NativeTransactionPool<P>,
	/// The block time used by the node.
	block_time: Duration,
	/// Keeps reth alive, this is used to ensure that the node does not exit
	/// while we are still using it.
	node_handle: Box<dyn Any + Send>,
}

impl<P, C> LocalNode<P, C>
where
	P: PlatformWithRpcTypes,
	C: ConsensusDriver<P>,
{
	/// Unless otherwise specified, The payload building process will be given
	/// this amount of time to return a payload. Also blocks cannot have lower
	/// block times than this because the timestamp resolution is in seconds.
	const MIN_BLOCK_TIME: Duration = Duration::from_secs(1);

	/// Creates a new local node instance with the given consensus driver and
	/// reth node configuration specific to the platform and network.
	pub async fn new<NodeBuilderFn, T, CB, AO>(
		consensus: C,
		chainspec: types::ChainSpec<P>,
		build_node: NodeBuilderFn,
	) -> eyre::Result<Self>
	where
		NodeBuilderFn:
			FnOnce(
				WithLaunchContext<
					NodeBuilder<Arc<TempDatabase<DatabaseEnv>>, types::ChainSpec<P>>,
				>,
			) -> WithLaunchContext<NodeBuilderWithComponents<T, CB, AO>>,
		T: FullNodeTypes<Types = P::NodeTypes>,
		CB: NodeComponentsBuilder<T>,
		CB::Components: NodeComponents<T, Pool: traits::PoolBounds<P>>,
		AO: RethRpcAddOns<NodeAdapter<T, CB::Components>> + 'static,
		EngineNodeLauncher: LaunchNode<
				NodeBuilderWithComponents<T, CB, AO>,
				Node = NodeHandle<NodeAdapter<T, CB::Components>, AO>,
			>,
	{
		let (rpc_ready_tx, rpc_ready_rx) = oneshot::channel::<()>();
		let tasks = TaskManager::new(tokio::runtime::Handle::current());
		let node_builder = create_node_builder::<P>(tasks.executor(), chainspec);
		let node_builder = build_node(node_builder).on_rpc_started(move |_, _| {
			let _ = rpc_ready_tx.send(());
			Ok(())
		});

		let config = node_builder.config().clone();
		let node_handle = node_builder.launch();
		let node_handle = Box::pin(node_handle).await?;

		let pool = node_handle.node.pool.clone();
		let pool = NativeTransactionPool::new(Arc::new(pool));
		let state_provider = Arc::new(node_handle.node.provider.clone());
		let canon_updates = Arc::new(node_handle.node.provider.clone());
		let exit_future = node_handle.node_exit_future;
		let boxed_handle = Box::new(node_handle.node);
		let node_handle: Box<dyn Any + Send> = boxed_handle;

		// Wait for the RPC server to be ready before returning
		rpc_ready_rx.await?;

		let provider =
			ProviderBuilder::<Identity, Identity, types::RpcTypes<P>>::default()
				.connect_ipc(config.rpc.ipcpath.clone().into())
				.await?;

		Ok(Self {
			consensus,
			exit_future,
			config,
			pool,
			provider,
			canon_updates,
			state_provider,
			tasks: Some(tasks),
			block_time: Self::MIN_BLOCK_TIME,
			node_handle,
		})
	}

	/// Sets the time interval payload builders are given to build a new payload.
	/// This is the interval between forkchoiceupdate engine api call and
	/// getpayload call from the CL node.
	pub fn set_block_time(&mut self, block_time: Duration) {
		self.block_time = block_time.max(Self::MIN_BLOCK_TIME);
	}

	/// The time interval between `engine_forkchoiceUpdate` and
	/// `engine_getPayload` Engine API calls, also specifies the timestamp that
	/// is assigned to the `payloadAttributes` for the next block.
	pub const fn block_time(&self) -> Duration {
		self.block_time
	}

	/// Returns the node configuration used to create this local node.
	pub const fn config(&self) -> &NodeConfig<types::ChainSpec<P>> {
		&self.config
	}

	/// Gets the chain ID as defined in the test Genesis block config.
	pub fn chain_id(&self) -> u64 {
		self.config().chain.chain_id()
	}

	/// Returns a provider connected to this local node instance over rpc.
	pub const fn provider(&self) -> &RootProvider<types::RpcTypes<P>> {
		&self.provider
	}

	/// Returns a state provider that has access to the node local state.
	pub const fn state_provider(&self) -> &Arc<dyn StateProviderFactory> {
		&self.state_provider
	}

	/// Returns a type that allows subscription to canonical chain updates.
	pub const fn canonical_chain_updates(
		&self,
	) -> &Arc<dyn CanonStateSubscriptions<Primitives = types::Primitives<P>>> {
		&self.canon_updates
	}

	/// Returns a reference to the native transaction pool.
	pub(crate) const fn pool(&self) -> &NativeTransactionPool<P> {
		&self.pool
	}

	pub const fn node_handle(&self) -> &Box<dyn Any + Send> {
		&self.node_handle
	}

	/// Returns a future that resolves when the node is shutting down gracefully.
	pub fn on_shutdown(&self) -> Shutdown {
		self.task_manager().executor().on_shutdown_signal().clone()
	}

	/// Access to the task manager managing the test node.
	/// # Panics
	/// Should never panic because `self.tasks` is None only after `drop`.
	pub fn task_manager(&self) -> &TaskManager {
		self.tasks.as_ref().expect("TaskManager must be present")
	}

	/// Returns a connected client to the RPC endpoint of this local node via IPC.
	pub async fn rpc_client(&self) -> Result<RcpClient, IpcError> {
		IpcClientBuilder::default()
			.build(&self.config.rpc.ipcpath)
			.await
	}

	/// Returns a reference to the node's consensus driver.
	/// In most cases you will not want to use this method directly, but rather
	/// use the `next_block` method to trigger the building of a new block.
	pub const fn consensus(&self) -> &C {
		&self.consensus
	}

	/// Triggers the building of a new block on this node with default parameters
	/// and returns the newly built block.
	pub async fn next_block(&self) -> eyre::Result<types::BlockResponse<P>> {
		self.next_block_with_params(C::Params::default()).await
	}

	/// Triggers the building of a new block on this node with user provided
	/// parameters and returns the newly built block.
	pub async fn next_block_with_params(
		&self,
		params: C::Params,
	) -> eyre::Result<types::BlockResponse<P>> {
		let latest_block = self
			.provider()
			.get_block_by_number(BlockNumberOrTag::Latest)
			.await?
			.ok_or_else(|| eyre::eyre!("Failed to get latest block from the node"))?;

		let latest_timestamp =
			Duration::from_secs(latest_block.header().timestamp());

		// calculate the timestamp for the new block
		let current_timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?;
		let elapsed_time = current_timestamp.saturating_sub(latest_timestamp);
		let target_timestamp = latest_timestamp + self.block_time + elapsed_time;

		// start the payload building process
		let payload_id = self
			.consensus
			.start_building(self, target_timestamp.as_secs(), &params)
			.await?;

		// wait for the payload to be built on the EL node.
		tokio::time::sleep(self.block_time).await;

		let block = self
			.consensus
			.finish_building(self, payload_id, &params)
			.await?;

		Ok(block)
	}

	/// Creates a new transaction builder compatible with this local node.
	pub fn build_tx(&self) -> types::TransactionRequest<P> {
		types::TransactionRequest::<P>::default()
			.with_chain_id(self.chain_id())
			.with_random_funded_signer()
			.with_gas_limit(210_000)
	}

	/// Sends a transaction built using the builder obtained from `build_tx` to
	/// the local node RPC endpoint.
	///
	/// Use this method if the transaction's signer is one of the known funded
	/// accounts. If the transaction's signer is not one of the known funded
	/// accounts, use `sign_and_send_tx` instead and provide the signer
	/// explicitly.
	pub async fn send_tx(
		&self,
		request: impl TransactionBuilder<types::RpcTypes<P>>,
	) -> eyre::Result<PendingTransactionBuilder<types::RpcTypes<P>>> {
		let Some(from) = request.from() else {
			return Err(eyre::eyre!(
				"Transaction request must have a 'from' field, use sign_and_send_tx \
				 instead"
			));
		};

		let Some(signer) = FundedAccounts::by_address(from) else {
			return Err(eyre::eyre!("No funded account found for address: {}", from));
		};

		self.sign_and_send_tx(request, signer).await
	}

	/// Sends a transaction built using the builder obtained from `build_tx` to
	/// the local node RPC endpoint. This method will sign the transaction
	/// using the provided signer.
	pub async fn sign_and_send_tx(
		&self,
		request: impl TransactionBuilder<types::RpcTypes<P>>,
		signer: impl TxSignerSync<Signature> + Send + Sync + 'static,
	) -> eyre::Result<PendingTransactionBuilder<types::RpcTypes<P>>> {
		let request = request.with_from(signer.address());

		// if nonce is not explicitly set, fetch it from the provider
		let request = match request.nonce() {
			Some(_) => request,
			None => request.with_nonce(
				self
					.provider
					.get_transaction_count(signer.address())
					.pending()
					.await?,
			),
		};

		let request = if let Some(gas_price) = request.gas_price() {
			request.with_gas_price(gas_price)
		} else {
			let gas_price = self.provider().get_gas_price().await?;
			request.with_gas_price(gas_price)
		};

		let request = match (
			request.max_fee_per_gas(),
			request.max_priority_fee_per_gas(),
		) {
			(Some(max_fee), Some(max_priority_fee)) => request
				.with_max_fee_per_gas(max_fee)
				.with_max_priority_fee_per_gas(max_priority_fee),
			(None, Some(_)) => {
				let fee_estimate = self.provider().estimate_eip1559_fees().await?;
				request.with_max_fee_per_gas(fee_estimate.max_fee_per_gas)
			}
			(Some(_), None) => {
				let fee_estimate = self.provider().estimate_eip1559_fees().await?;
				request
					.with_max_priority_fee_per_gas(fee_estimate.max_priority_fee_per_gas)
			}
			(None, None) => {
				let fee_estimate = self.provider().estimate_eip1559_fees().await?;
				request
					.with_max_fee_per_gas(fee_estimate.max_fee_per_gas)
					.with_max_priority_fee_per_gas(fee_estimate.max_priority_fee_per_gas)
			}
		};

		let mut tx = request.build_unsigned()?;
		let signature = signer.sign_transaction_sync(&mut tx)?;
		let envelope: types::TxEnvelope<P> = tx.into_signed(signature).into();
		let encoded = envelope.encoded_2718();

		self
			.provider()
			.send_raw_transaction(&encoded)
			.await
			.map_err(Into::into)
	}
}

impl<P, C> Drop for LocalNode<P, C>
where
	P: PlatformWithRpcTypes,
	C: ConsensusDriver<P>,
{
	fn drop(&mut self) {
		if let Some(tasks) = self.tasks.take() {
			tasks.graceful_shutdown_with_timeout(Duration::from_secs(3));
			std::fs::remove_dir_all(self.config.datadir().to_string())
				.unwrap_or_else(|e| {
					panic!(
						"Failed to remove temporary data directory {}: {e}",
						self.config.datadir()
					)
				});
		}
	}
}

impl<P, C> Future for LocalNode<P, C>
where
	P: PlatformWithRpcTypes,
	C: ConsensusDriver<P>,
{
	type Output = eyre::Result<()>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		self.get_mut().exit_future.poll_unpin(cx)
	}
}

unsafe impl<P, C> Sync for LocalNode<P, C>
where
	P: PlatformWithRpcTypes,
	C: ConsensusDriver<P>,
{
}

unsafe impl<P, C> Send for LocalNode<P, C>
where
	P: PlatformWithRpcTypes,
	C: ConsensusDriver<P>,
{
}

fn create_node_builder<P: Platform>(
	executor: TaskExecutor,
	chainspec: types::ChainSpec<P>,
) -> WithLaunchContext<
	NodeBuilder<Arc<TempDatabase<DatabaseEnv>>, types::ChainSpec<P>>,
>
where
	types::ChainSpec<P>: EthChainSpec,
{
	let tempdir = std::env::temp_dir();
	let random_id = nanoid!();
	let data_path = tempdir.join(format!("rblib.{random_id}.datadir"));

	std::fs::create_dir_all(&data_path)
		.expect("Failed to create temporary data directory");

	let rpc_ipc_path = tempdir.join(format!("rblib.{random_id}.rpc-ipc"));
	let auth_ipc_path = tempdir.join(format!("rblib.{random_id}.auth-ipc"));

	let mut rpc = RpcServerArgs::default().with_auth_ipc();
	rpc.ws = false;
	rpc.http = false;
	rpc.auth_port = 0;
	rpc.ipcpath = rpc_ipc_path.to_string_lossy().into();
	rpc.auth_ipc_path = auth_ipc_path.to_string_lossy().into();

	let mut network = NetworkArgs::default().with_unused_ports();
	network.discovery.disable_discovery = true;

	let datadir = DatadirArgs {
		datadir: data_path
			.to_string_lossy()
			.parse()
			.expect("Failed to parse data dir path"),
		static_files_path: None,
	};

	let db_path = datadir
		.datadir
		.unwrap_or_chain_default(chainspec.chain(), datadir.clone())
		.db();

	let config = NodeConfig::new(Arc::new(chainspec))
		.with_datadir_args(datadir)
		.with_rpc(rpc)
		.with_network(network);

	NodeBuilder::new(config)
		.with_database(Arc::new(TempDatabase::new(
			init_db(
				db_path.as_path(),
				DatabaseArguments::new(ClientVersion::default())
					.with_max_read_transaction_duration(Some(
						MaxReadTransactionDuration::Unbounded,
					))
					.with_geometry_max_size(Some(4 * MEGABYTE))
					.with_growth_step(Some(4 * KILOBYTE)),
			)
			.expect(ERROR_DB_CREATION),
			db_path,
		)))
		.with_launch_context(executor)
}
