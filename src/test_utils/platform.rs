use {
	super::{ConsensusDriver, LocalNode},
	crate::prelude::*,
};

/// This trait is used to automatically select the correct local test node type
/// based on the platform that is being tested. This is implemented for
/// [`Ethereum`] and [`Optimism`] platforms. SDK users can implement this trait
/// for their own custom platforms.
///
/// Example usage:
///
/// Create ethereum test node:
/// ```rust
/// let pipeline = Pipeline::default();
/// let node = Ethereum::create_test_node(pipeline).await?;
/// ```
///
/// Create optimism test node:
/// ```rust
/// let pipeline = Pipeline::default();
/// let node = Optimism::create_test_node(pipeline).await?;
/// ```
///
/// Generic instantiation:
/// ```rust
/// use crate::test_utils::TestablePlatform;
/// fn run_test<T: TestablePlatform>() -> eyre::Result<()> {
/// 	let pipeline = Pipeline::default();
/// 	let node = T::create_test_node(pipeline).await?;
/// 	Ok(())
/// }
/// ```
///
/// This trait is used in the `rblib_test` macro to automatically create test
/// variants of all internal unit tests for all platforms. You can use
/// [`rblib_test`] with externally defined platforms as long as they implement
/// this trait.
pub trait TestNodeFactory<P: PlatformWithRpcTypes> {
	type ConsensusDriver: ConsensusDriver<P>;
	type CliExtArgs: Default + Send + Sync;

	/// Using the platform definition type alone and a pipeline this will create a
	/// fully functional [`LocalNode`] that is an in-process reth node with the
	/// pipeline configured as they payload builder.
	///
	/// The test node has all the battery included, for accessing the node RPC
	/// interface, triggering the EL <-> CL payload building protocol, and so on.
	/// See docs for [`LocalNode`] for more details.
	///
	/// This variant will use the default CLI arguments for the platform.
	fn create_test_node(
		pipeline: Pipeline<P>,
	) -> impl Future<Output = eyre::Result<LocalNode<P, Self::ConsensusDriver>>>
	{
		Self::create_test_node_with_args(pipeline, Self::CliExtArgs::default())
	}

	/// Using the platform definition type alone and a pipeline this will create a
	/// fully functional [`LocalNode`] that is an in-process reth node with the
	/// pipeline configured as they payload builder.
	///
	/// The test node has all the battery included, for accessing the node RPC
	/// interface, triggering the EL <-> CL payload building protocol, and so on.
	/// See docs for [`LocalNode`] for more details.
	///
	/// This variant allows to pass the `Ext` type that is used to extend the reth
	/// node cli arguments.
	fn create_test_node_with_args(
		pipeline: Pipeline<P>,
		args: Self::CliExtArgs,
	) -> impl Future<Output = eyre::Result<LocalNode<P, Self::ConsensusDriver>>>;
}

/// A helper trait that is automatically implemented for all platform
/// implementations that have a `TestNodeFactory` implementation and rpc types
///
/// This allows the `rblib_test` macro to automatically synthesize tests for
/// many platforms without needing to create separate versions of the test for
/// each platform.
pub trait TestablePlatform:
	PlatformWithRpcTypes + TestNodeFactory<Self>
{
}

/// Blanket implementation for all platforms that implement `Platform` and
/// `NetworkSelector` and `TestNodeFactory`.
impl<T> TestablePlatform for T where T: PlatformWithRpcTypes + TestNodeFactory<T>
{}
