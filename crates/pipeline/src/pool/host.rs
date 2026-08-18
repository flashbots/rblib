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
		transaction_pool::{
			NewTransactionEvent,
			TransactionListenerKind,
			TransactionPool,
		},
	},
	std::sync::OnceLock,
	tokio::sync::mpsc::{Receiver, error::TryRecvError},
	tracing::debug,
};

/// Stream of transactions accepted by the transaction pool of the host node.
type NewTransactions<P> =
	Receiver<NewTransactionEvent<<P as Platform>::PooledTransaction>>;

/// The `tracing` target of the mempool listener lines, shared with the
/// lifecycle log.
const LIFECYCLE_TARGET: &str = "rblib::pool::lifecycle";

/// The most queued listener events recorded before a canonical chain event is
/// processed. Reth's listener channel holds 1024 events and drops on a full
/// channel, so the bound is never reached in practice; it only guarantees that
/// the drain terminates while transactions keep arriving.
const MAX_LISTENER_DRAIN: usize = 4096;

/// The state of the mempool listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerState {
	/// The listener is connected and its arm is polled.
	Open,
	/// The sending side of the listener is gone: no event will ever arrive
	/// again, the arm is disabled and the loss was reported once.
	Closed,
}

/// Records every event the listener has already queued, without waiting for
/// more, bounded by [`MAX_LISTENER_DRAIN`]. Stops on an empty channel
/// (`Open`) or on a disconnected one (`Closed`, after the buffered events were
/// recorded).
fn drain_ready<T>(
	receiver: &mut Receiver<T>,
	mut record: impl FnMut(T),
) -> ListenerState {
	(0..MAX_LISTENER_DRAIN)
		.find_map(|_| {
			receiver.try_recv().map_or_else(
				|error| match error {
					TryRecvError::Empty => Some(ListenerState::Open),
					TryRecvError::Disconnected => Some(ListenerState::Closed),
				},
				|event| {
					record(event);
					None
				},
			)
		})
		// the bound was reached with events still queued: the listener is open
		.unwrap_or(ListenerState::Open)
}

/// Disables the mempool listener arm and reports the loss once.
fn close_listener(listener: &mut ListenerState) {
	*listener = ListenerState::Closed;
	tracing::warn!(
		target: LIFECYCLE_TARGET,
		"mempool listener closed, arrival timestamps will no longer be recorded"
	);
}

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

		// subscribe to new mempool transactions before anything else happens on
		// the node, so that the arrival of every transaction is observed.
		let system_pool = NativeTransactionPool::new(system_pool);
		let new_transactions =
			system_pool.new_transactions_listener_for(TransactionListenerKind::All);

		self
			.instances
			.set(Instances {
				system_pool,
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
			Arc::clone(self).maintenance_loop(new_transactions),
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
	/// Records the arrival of a transaction accepted by the host node pool.
	/// The pool stamps the transaction with the moment it was validated, which
	/// is when the transaction was received by the node.
	fn record_arrival(
		order_pool: &OrderPool<P>,
		event: &NewTransactionEvent<P::PooledTransaction>,
	) {
		order_pool.report_mempool_transaction(
			*event.transaction.hash(),
			event.transaction.timestamp,
		);
	}

	async fn maintenance_loop(
		self: Arc<Self>,
		mut new_transactions: NewTransactions<P>,
	) {
		let instances = self.instances.get().expect("HostNode must be attached");
		let mut chain_events = instances.canonical_sub.canonical_state_stream();
		let mut shutdown = instances.shutdown.clone();
		let mut listener = ListenerState::Open;

		loop {
			// The arms are polled fairly. The mempool listener must not take
			// priority over the canonical stream: reth's canonical state stream is
			// broadcast backed and silently drops lagged notifications, so a
			// starved canonical arm would miss blocks (tip header, eviction of
			// mined orders). The canonical arm drains the queued listener events
			// itself before it processes a block, see there.
			tokio::select! {
				// Reth node shutdown signal. Terminate the maintenance loop.
				() = &mut shutdown => {
					break;
				}

				// A transaction was accepted by the host node transaction pool. A
				// closed listener disables this arm and is reported once.
				event = new_transactions.recv(), if listener == ListenerState::Open => {
					event.map_or_else(
						|| close_listener(&mut listener),
						|event| Self::record_arrival(&instances.order_pool, &event),
					);
				}

				// Changes to the canonical chain, reverts, reorgs, commits, etc.
				Some(event) = chain_events.next() => {
					// The listener event of a transaction is queued before any block
					// that can contain it, so recording the queued arrivals first
					// orders received before mined within this task, without giving
					// the listener priority over canonical processing.
					if listener == ListenerState::Open {
						let drained = drain_ready(&mut new_transactions, |event| {
							Self::record_arrival(&instances.order_pool, &event);
						});
						match drained {
							ListenerState::Open => {}
							ListenerState::Closed => close_listener(&mut listener),
						}
					}

					// blocks reverted by a reorg first: transactions mined in them are
					// no longer mined and may be mined again in the committed blocks
					event
						.reverted()
						.iter()
						.flat_map(|reverted| reverted.blocks().values())
						.for_each(|block| instances.order_pool.report_reverted_block(block));

					// remove orders that have transactions included in the committed block
					for block in event.committed().blocks().values() {
						instances.order_pool.report_committed_block(block);
					}

					// update the tip header with the latest block header
					*instances.tip_header.write() = event.tip().sealed_header().clone();
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

		let system_pool = node.pool().clone();
		let new_transactions =
			system_pool.new_transactions_listener_for(TransactionListenerKind::All);

		self
			.instances
			.set(Instances {
				order_pool,
				system_pool,
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
			Arc::clone(self).maintenance_loop(new_transactions),
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

#[cfg(test)]
mod tests {
	use super::*;

	/// A channel with `queued` events already sent, and its sender.
	fn queued(
		capacity: usize,
		queued: usize,
	) -> (tokio::sync::mpsc::Sender<usize>, Receiver<usize>) {
		let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
		(0..queued).for_each(|event| sender.try_send(event).expect("room"));
		(sender, receiver)
	}

	#[test]
	fn drain_records_the_queued_events_and_stops_on_an_empty_channel() {
		let (sender, mut receiver) = queued(8, 3);
		let mut recorded = Vec::new();

		let state = drain_ready(&mut receiver, |event| recorded.push(event));
		assert_eq!(state, ListenerState::Open);
		assert_eq!(recorded, vec![0, 1, 2]);

		// nothing queued: nothing recorded, the listener is still open
		let state = drain_ready(&mut receiver, |event| recorded.push(event));
		assert_eq!(state, ListenerState::Open);
		assert_eq!(recorded, vec![0, 1, 2]);
		drop(sender);
	}

	#[test]
	fn drain_reports_a_disconnected_listener_after_its_buffered_events() {
		let (sender, mut receiver) = queued(8, 2);
		drop(sender);
		let mut recorded = Vec::new();

		// the buffered events are recorded before the disconnect is reported
		let state = drain_ready(&mut receiver, |event| recorded.push(event));
		assert_eq!(state, ListenerState::Closed);
		assert_eq!(recorded, vec![0, 1]);

		// and it stays closed
		let state = drain_ready(&mut receiver, |event| recorded.push(event));
		assert_eq!(state, ListenerState::Closed);
		assert_eq!(recorded, vec![0, 1]);
	}

	#[test]
	fn drain_is_bounded_and_leaves_the_rest_for_the_next_round() {
		let (sender, mut receiver) =
			queued(2 * MAX_LISTENER_DRAIN, MAX_LISTENER_DRAIN + 1);
		let mut recorded = 0;

		// exactly the bound is recorded, the listener is still open
		let state = drain_ready(&mut receiver, |_| recorded += 1);
		assert_eq!(state, ListenerState::Open);
		assert_eq!(recorded, MAX_LISTENER_DRAIN);

		// the next drain records the remaining event
		let state = drain_ready(&mut receiver, |_| recorded += 1);
		assert_eq!(state, ListenerState::Open);
		assert_eq!(recorded, MAX_LISTENER_DRAIN + 1);
		drop(sender);
	}
}
