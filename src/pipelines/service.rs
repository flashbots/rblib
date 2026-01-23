//! This module implements the machinery that takes a declarative pipeline
//! definition and turns it into a Reth compatible payload builder.

use {
	super::{job::PayloadJob, metrics},
	crate::{
		alloy::primitives::B256,
		prelude::*,
		reth::{
			api::PayloadBuilderAttributes,
			builder::{
				BuilderContext,
				NodeConfig,
				components::PayloadServiceBuilder,
			},
			node::builder::NodePrimitives,
			payload::builder::{PayloadBuilderHandle, PayloadBuilderService, *},
			providers::{CanonStateSubscriptions, StateProviderFactory},
		},
	},
	reth_ethereum::provider::CanonStateNotification,
	std::sync::Arc,
	tracing::debug,
};

/// This type is the bridge between Reth's payload builder API and the
/// pipelines API. It will take a pipeline instance and turn it into what
/// eventually becomes a Payload Job Generator. The payload Job Generator
/// will be responsible for creating new [`PayloadJob`] instances
/// whenever a new payload request comes in from the CL Node.
pub(super) struct PipelineServiceBuilder<P: Platform> {
	pipeline: Pipeline<P>,
}

impl<P: Platform> PipelineServiceBuilder<P> {
	pub(super) fn new(pipeline: Pipeline<P>) -> Self {
		Self { pipeline }
	}
}

impl<Plat, Node, Pool> PayloadServiceBuilder<Node, Pool, types::EvmConfig<Plat>>
	for PipelineServiceBuilder<Plat>
where
	Plat: Platform,
	Node: traits::NodeBounds<Plat>,
	Pool: traits::PoolBounds<Plat>,
{
	async fn spawn_payload_builder_service(
		self,
		ctx: &BuilderContext<Node>,
		_: Pool,
		_: types::EvmConfig<Plat>,
	) -> eyre::Result<PayloadBuilderHandle<types::PayloadTypes<Plat>>> {
		let pipeline = self.pipeline;
		debug!("Spawning payload builder service for: {pipeline:#?}");

		let provider = Arc::new(ctx.provider().clone());

		let metrics =
			metrics::Payload::with_scope(&format!("{}_payloads", pipeline.name()));

		let service = ServiceContext {
			provider: Arc::clone(&provider),
			node_config: ctx.config().clone(),
		};

		// assign metric names to each step in the pipeline.
		// This will be used to automatically generate runtime observability data
		// during pipeline runs. Also call an optional `setup` function on the step
		// that gives it a chance to perform any initialization it needs before
		// processing any payload jobs.
		let provider = provider as Arc<dyn StateProviderFactory>;

		for step in pipeline.iter_steps() {
			let navi = step.navigator(&pipeline).expect(
				"Invalid step path. This is a bug in the pipeline executor \
				 implementation.",
			);

			let metrics_scope = format!("{}_step_{}", pipeline.name(), navi.path());
			navi.instance().init_metrics(&metrics_scope);

			let init_ctx = InitContext::new(Arc::clone(&provider), metrics_scope);
			navi.instance().setup(init_ctx).await?;
		}

		let (service, builder) = PayloadBuilderService::new(
			JobGenerator::new(pipeline, service, metrics),
			ctx.provider().canonical_state_stream(),
		);

		ctx
			.task_executor()
			.spawn_critical("payload_builder_service", Box::pin(service));

		Ok(builder)
	}
}

/// There is one service context instance per reth node. This type gives
/// individual jobs access to the node state, transaction pool and other
/// runtime facilities that are managed by reth.
struct ServiceContext<Plat, Provider>
where
	Plat: Platform,
	Provider: traits::ProviderBounds<Plat>,
{
	provider: Arc<Provider>,
	node_config: NodeConfig<types::ChainSpec<Plat>>,
}

impl<Plat, Provider> ServiceContext<Plat, Provider>
where
	Plat: Platform,
	Provider: traits::ProviderBounds<Plat>,
{
	fn provider(&self) -> &Provider {
		&self.provider
	}

	const fn node_config(&self) -> &NodeConfig<types::ChainSpec<Plat>> {
		&self.node_config
	}

	const fn chain_spec(&self) -> &Arc<types::ChainSpec<Plat>> {
		&self.node_config().chain
	}
}

/// This type is stored inside the [`PayloadBuilderService`] type in Reth.
/// There's one instance of this type per node, and it is instantiated during
/// the node startup inside `spawn_payload_builder_service`.
///
/// The responsibility of this type is to respond to new payload requests when
/// FCU calls come from the CL Node. Each FCU call will generate a new
/// `PayloadID` on its side and will pass it to the `new_payload_job` method.
pub(super) struct JobGenerator<Plat, Provider>
where
	Plat: Platform,
	Provider: traits::ProviderBounds<Plat>,
{
	pipeline: Arc<Pipeline<Plat>>,
	service: Arc<ServiceContext<Plat, Provider>>,
	metrics: Arc<metrics::Payload>,
	pre_cached: Option<PrecachedState>,
}

impl<Plat, Provider> JobGenerator<Plat, Provider>
where
	Plat: Platform,
	Provider: traits::ProviderBounds<Plat>,
{
	fn new(
		pipeline: Pipeline<Plat>,
		service: ServiceContext<Plat, Provider>,
		metrics: metrics::Payload,
	) -> Self {
		let pipeline = Arc::new(pipeline);
		let service = Arc::new(service);

		Self {
			pipeline,
			service,
			metrics: Arc::new(metrics),
			pre_cached: None,
		}
	}

	/// Returns the pre-cached reads for the given parent header if it matches the
	/// cached state's block.
	fn maybe_pre_cached(&self, parent: B256) -> Option<ExecutionCache> {
		// TODO: probably possible to remove .clone() here, even tho it shouldn't be
		// too heavy
		if let Some(cached) = self.pre_cached.clone()
			&& cached.block == parent
		{
			return Some(cached.cached);
		}
		None
	}
}

impl<Plat, Provider> PayloadJobGenerator for JobGenerator<Plat, Provider>
where
	Plat: Platform,
	Provider: traits::ProviderBounds<Plat>,
{
	type Job = PayloadJob<Plat>;

	fn new_payload_job(
		&self,
		attribs: types::PayloadBuilderAttributes<Plat>,
	) -> Result<Self::Job, PayloadBuilderError> {
		let header = self
			.service
			.provider()
			.sealed_header_by_hash(attribs.parent())?
			.ok_or_else(|| {
				PayloadBuilderError::MissingParentHeader(attribs.parent())
			})?;

		let hash = header.hash();
		let factory: Arc<Provider> = Arc::new(self.service.provider().clone());

		// This is the beginning of the state manipulation API usage from within
		// the pipelines API.
		let block_ctx = BlockContext::new(
			header,
			attribs,
			factory,
			self.service.chain_spec().clone(),
			self.maybe_pre_cached(hash),
		)
		.map_err(PayloadBuilderError::other)?;

		// Job should complete within deadline (12s) even if GetPayload is never
		// called. This prevents job accumulation.
		// TODO: when FCU update avalanche is fixed we could replace
		// builder.deadline with payload.attributes.timeout
		let deadline = self.service.node_config().builder.deadline;

		Ok(PayloadJob::new(
			&self.pipeline,
			block_ctx,
			self.metrics.clone(),
			deadline,
		))
	}

	fn on_new_state<N: NodePrimitives>(
		&mut self,
		new_state: CanonStateNotification<N>,
	) {
		let cached = ExecutionCache::default();
		if let Err(()) =
			cached.insert_state(&new_state.committed().execution_outcome().bundle)
		{
			tracing::error!(
				"Failed to insert committed state bundle for block {}",
				new_state.tip().hash()
			);
			return;
		}
		self.pre_cached = Some(PrecachedState {
			block: new_state.tip().hash(),
			cached,
		});
	}
}

/// Pre-filled [`ExecutionCache`] for a specific block.
///
/// This is extracted from the [`CanonStateNotification`] for the tip block.
#[derive(Debug, Clone)]
pub(super) struct PrecachedState {
	/// The block for which the state is pre-cached.
	pub block: B256,
	/// Cached state for the block.
	pub cached: ExecutionCache,
}
