use {
	super::*,
	crate::reth,
	core::{
		pin::Pin,
		task::{Context, Poll},
	},
	futures::{Stream, StreamExt},
	reth::{
		chainspec::EthChainSpec,
		errors::ProviderError,
		node::builder::FullNodeComponents,
		primitives::SealedHeader,
		providers::{
			BlockReaderIdExt,
			CanonStateSubscriptions,
			ChainSpecProvider,
			StateProvider,
			StateProviderFactory,
		},
	},
	tokio::sync::watch,
	tokio_stream::wrappers::WatchStream,
	tracing::{debug, warn},
};

/// This type represents the latest canonical state of the chain.
///
/// Everytime the chain we're building for advances its state an instance of
/// this type is created that allows access to the state of the chain at the new
/// tip.
pub struct CanonicalState<P: Platform> {
	pub header: SealedHeader<types::Header<P>>,
	pub state: Box<dyn StateProvider>,
}

/// This type is responsible for tracking the canonical state of the chain and
/// producing new `CanonicalState` instance for every new comitted block or
/// chain reorg.
pub struct CanonicalStateSource<P: Platform> {
	latest: Arc<CanonicalState<P>>,
	receiver: WatchStream<Arc<CanonicalState<P>>>,
}

impl<P: Platform> CanonicalStateSource<P> {
	/// Returns the latest known canonical state of the chain.
	pub fn latest(&self) -> &CanonicalState<P> {
		&self.latest
	}
}

impl<P: Platform> Stream for CanonicalStateSource<P> {
	type Item = Arc<CanonicalState<P>>;

	fn poll_next(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
	) -> Poll<Option<Self::Item>> {
		let this = self.get_mut();

		let poll_res = this.receiver.poll_next_unpin(cx);
		if let Poll::Ready(Some(state)) = &poll_res {
			this.latest = state.clone();
		}

		poll_res
	}
}

impl<P: Platform> CanonicalStateSource<P> {
	/// Creates a new `CanonicalStateSource` instance from a reth node instance.
	pub fn reth<Node>(node: &Node) -> Result<Self, ProviderError>
	where
		Node: FullNodeComponents<Types = types::NodeTypes<P>>,
	{
		let header = node.provider().latest_header()?.unwrap_or_else(|| {
			let chainspec = node.provider().chain_spec();
			SealedHeader::new(
				chainspec.genesis_header().clone(),
				chainspec.genesis_hash(),
			)
		});

		let state = node.provider().state_by_block_hash(header.hash())?;
		let initial_state = Arc::new(CanonicalState { header, state });

		let latest = initial_state.clone();
		let (sender, receiver) = watch::channel(initial_state);
		let receiver = WatchStream::new(receiver);

		tokio::spawn(reth_forward_loop(node, sender));

		Ok(Self { latest, receiver })
	}
}

fn reth_forward_loop<P, Node>(
	node: &Node,
	sender: watch::Sender<Arc<CanonicalState<P>>>,
) -> impl Future<Output = ()> + 'static
where
	P: Platform,
	Node: FullNodeComponents<Types = types::NodeTypes<P>>,
{
	let provider = node.provider().clone();
	let mut canon_state = provider.canonical_state_stream();
	let mut shutdown = node.task_executor().on_shutdown_signal().clone();

	async move {
		loop {
			tokio::select! {
				() = &mut shutdown => {
					debug!("CanonicalStateSource: shutting down");
					break;
				}

				Some(update) = canon_state.next() => {
					let header = update.tip().sealed_header().clone();
					let state = match provider.state_by_block_hash(header.hash()) {
						Ok(state) => state,
						Err(err) => {
							warn!("failed to get state for new chain tip {}, err: {err}", header.hash());
							continue;
						}
					};

					let new_state = Arc::new(CanonicalState {
						header,
						state,
					});

					if sender.send(new_state).is_err() {
						break;
					}
				}
			}
		}
	}
}
