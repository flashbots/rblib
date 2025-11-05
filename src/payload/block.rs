use {
	crate::{
		prelude::*,
		reth::{
			api::ConfigureEvm,
			evm::{block::BlockExecutionError, execute::BlockBuilder},
			primitives::SealedHeader,
			providers::{StateProvider, StateProviderBox},
			revm::{State, database::StateProviderDatabase},
		},
	},
	std::sync::Arc,
	thiserror::Error,
};

#[derive(Debug, Error)]
pub enum Error<P: Platform> {
	#[error("Evm configuration error: {0}")]
	EvmEnv(types::EvmEnvError<P>),

	#[error("Block execution error: {0}")]
	BlockExecution(#[from] BlockExecutionError),
}

/// This type represents the beginning of the payload building process. It
/// captures all the state and configuration required to create subsequent
/// payload state transition checkpoints.
///
/// Usually instances of this type are created once as a response to CL client
/// `ForkchoiceUpdated` requests with `PayloadAttributes`.
/// This signals the need to start constructing a new payload on top of a given
/// block. Then different versions of payload checkpoints are created on top of
/// this instance using the [`BlockContext::start`] method.
///
/// This type is inexpensive to clone.
pub struct BlockContext<P: Platform> {
	inner: Arc<BlockContextInner<P>>,
}

impl<P: Platform> Clone for BlockContext<P> {
	fn clone(&self) -> Self {
		Self {
			inner: Arc::clone(&self.inner),
		}
	}
}

impl<P: Platform> PartialEq for BlockContext<P> {
	fn eq(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}
}

/// Constructors
impl<P: Platform> BlockContext<P> {
	/// To create a new [`BlockContext`], we need:
	///  - The parent block header on top of which we are building this block.
	///  - The payload builder attributes for the next block. This usually comes
	///    from [`PayloadJobGenerator::new_payload_job`] as a response to Engine
	///    API `ForkchoiceUpdated` requests.
	///  - The state of the chain at the parent block. This can be acquired from
	///    [`StateProviderFactory::state_by_block_hash`]
	///  - The chainspec of the chain we're building for.
	///
	/// [`PayloadJobGenerator::new_payload_job`]: reth_payload_builder::PayloadJobGenerator::new_payload_job
	/// [`StateProviderFactory::state_by_block_hash`]: reth::providers::StateProviderFactory::state_by_block_hash
	pub fn new(
		parent: SealedHeader<types::Header<P>>,
		attribs: types::PayloadBuilderAttributes<P>,
		base_state: StateProviderBox,
		chainspec: Arc<types::ChainSpec<P>>,
	) -> Result<Self, Error<P>> {
		let block_env = P::next_block_environment_context::<P>(
			&chainspec,
			parent.header(),
			&attribs,
		);

		let evm_config = P::evm_config::<P>(Arc::clone(&chainspec));
		let evm_env = evm_config
			.next_evm_env(&parent, &block_env)
			.map_err(Error::EvmEnv)?;

		let mut base_state = State::builder()
			.with_database(StateProviderDatabase(base_state))
			.with_bundle_update()
			.build();

		// prepare the base state for the next block
		evm_config
			.builder_for_next_block(&mut base_state, &parent, block_env.clone())
			.map_err(Error::EvmEnv)?
			.apply_pre_execution_changes()?;

		Ok(Self {
			inner: Arc::new(BlockContextInner {
				parent,
				attribs,
				evm_env,
				block_env,
				base_state,
				evm_config,
				chainspec,
			}),
		})
	}
}

/// Public Read API
impl<P: Platform> BlockContext<P> {
	/// Returns the parent block header of the block for which the payload is
	/// being built.
	pub fn parent(&self) -> &SealedHeader<types::Header<P>> {
		&self.inner.parent
	}

	/// Returns the payload attributes supplied by the CL client during the
	/// `ForkchoiceUpdated` request.
	pub fn attributes(&self) -> &types::PayloadBuilderAttributes<P> {
		&self.inner.attribs
	}

	/// Returns the state provider that provides access to the state of the
	/// environment rooted at the end of the parent block for which the payload
	/// is being built.
	pub fn base_state(&self) -> &dyn StateProvider {
		self.inner.base_state.database.as_ref()
	}

	/// Returns the EVM configuration used to create EVM instances for executing
	/// transactions in the payload under construction.
	pub fn evm_config(&self) -> &P::EvmConfig {
		&self.inner.evm_config
	}

	/// Returns an evm environment preconfigured for executing transactions in
	/// the payload under construction.
	pub fn evm_env(&self) -> &types::EvmEnv<P> {
		&self.inner.evm_env
	}

	/// Returns the context required for configuring the environment of the next
	/// block that is being built. This context contains information that
	/// cannot be derived from the parent block header.
	pub fn block_env(&self) -> &types::NextBlockEnvContext<P> {
		&self.inner.block_env
	}

	/// Returns the chainspec that defines the chain- and fork-specific parameters
	/// that are used to configure the EVM environment and the next block
	/// environment for the block that is being built.
	pub fn chainspec(&self) -> &Arc<types::ChainSpec<P>> {
		&self.inner.chainspec
	}
}

/// Public building API
impl<P: Platform> BlockContext<P> {
	/// Creates a new empty payload checkpoint rooted at the state of the parent
	/// block. Instances of the `Checkpoint` type can be used to progressively
	/// modify and simulate the payload under construction. Checkpoints are
	/// convertible to the final built payload at any time.
	///
	/// There may be multiple versions of the payload under construction, each
	/// call to this function creates a new fork of the payload building process.
	pub fn start(&self) -> Checkpoint<P> {
		Checkpoint::new_at_block(self.clone())
	}

	/// Given a payload checkpoint, this method builds a new payload on top of
	/// this block that is ready to be handed back to the CL client as a response
	/// to the `ForkchoiceUpdated` request.
	pub fn build_payload(
		payload: &Checkpoint<P>,
	) -> Result<types::BuiltPayload<P>, PayloadBuilderError> {
		payload.build_payload()
	}
}

struct BlockContextInner<P: Platform> {
	/// The parent block header of the block for which the payload is
	/// being built.
	parent: SealedHeader<types::Header<P>>,

	/// The payload attributes we've got from the CL client
	/// during the `ForkchoiceUpdated` request.
	attribs: types::PayloadBuilderAttributes<P>,

	/// Context object for constructing evm instance for the block being built.
	/// We use this context to apply any chain- and fork-specific state updates
	/// that are defined in the chainspec.
	evm_env: types::EvmEnv<P>,

	/// Context required for configuring this block environment.
	/// Contains information that can't be derived from the parent block.
	block_env: types::NextBlockEnvContext<P>,

	/// Access to the state of the environment rooted at the end of the parent
	/// block for which the payload is being built. This state will be read when
	/// there is an attempt to read a storage element that is not present in any
	/// of the checkpoints created on top of this block context.
	///
	/// This state has no changes made to it during the payload building process
	/// through any of the created checkpoints.
	base_state: State<StateProviderDatabase<StateProviderBox>>,

	/// The EVM factory configured for the environment in which we are building
	/// the payload. This type is used to create individual EVM instances that
	/// execute transactions in the payload under construction.
	evm_config: P::EvmConfig,

	/// The chainspec that defines the chain and fork specific parameters that
	/// are used to configure the EVM environment and the next block environment
	/// for the block that is being built.
	chainspec: Arc<types::ChainSpec<P>>,
}

impl<P: Platform> core::fmt::Debug for BlockContext<P> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		f.debug_struct("BlockContext")
			.field("parent", &self.inner.parent)
			.field("attribs", &self.inner.attribs)
			.finish()
	}
}
