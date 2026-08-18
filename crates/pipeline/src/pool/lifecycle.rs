//! Transaction lifecycle log
//!
//! Keeps track of the moments at which the order pool saw a transaction:
//! when it was received (from the host node mempool, as a member of a bundle
//! or as a standalone transaction order), when it was first considered for
//! inclusion in a payload, when it was first included in a built payload and
//! when it was mined in a canonical block. Transactions that leave the pool
//! without being mined are marked as dropped. When the block that contains a
//! mined transaction is reverted by a chain reorg the mined stage is cleared
//! again and the transaction can be mined once more in the new chain.
//!
//! Every transition is reported through `tracing` on the
//! `rblib::pool::lifecycle` target. Received, considered, included, dropped
//! and reverted transitions are logged at `debug` level, mined transactions at
//! `info` level together with the time elapsed between all stages, so that
//! `RUST_LOG=rblib::pool::lifecycle=info` yields one line per mined
//! transaction that the pool has seen.
//!
//! The stages of a transaction are reported by different tasks (the mempool
//! listener, the pipeline events listener and the canonical chain listener), so
//! a transaction can be considered for inclusion before its arrival was
//! recorded. The log therefore creates a provisional entry, without arrival
//! information, when a transaction is considered or included before it was
//! received, and fills in the arrival when it is reported later. Mined,
//! dropped and reverted stages never create entries: every block mines many
//! transactions that this pool has never seen.
//!
//! The log is a bounded window over recent transactions: entries whose last
//! activity is older than the configured retention are pruned (at most once
//! per tenth of the retention) when a block is committed to the chain or when
//! the log is full, and once the log holds the configured maximum number of
//! entries new transactions are only tracked after such a prune made room for
//! them, otherwise they are refused and counted. See the sizing notes on
//! [`TxLifecycleLog`] for what the defaults can sustain.

use {
	super::*,
	parking_lot::Mutex,
	reth::payload::builder::PayloadId,
	std::{
		sync::atomic::{AtomicU64, Ordering},
		time::{Duration, Instant, SystemTime, UNIX_EPOCH},
	},
	tracing::{debug, info},
};

/// The `tracing` target used for all transaction lifecycle log lines.
const TARGET: &str = "rblib::pool::lifecycle";

/// Where the order pool saw a transaction for the first time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxSource {
	/// The transaction was accepted by the transaction pool of the host Reth
	/// node.
	Mempool,

	/// The transaction is a member of a bundle order inserted into the order
	/// pool. Carries the hash of that bundle.
	Bundle(B256),

	/// The transaction was inserted into the order pool as a standalone
	/// transaction order (`Order::Transaction`).
	Transaction,
}

/// The moment a transaction reached a payload building stage (considered for
/// inclusion or included in a payload) and the payload job it happened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxPayloadStage {
	at: Instant,
	payload_id: PayloadId,
}

impl TxPayloadStage {
	/// When this stage was reached.
	pub const fn at(&self) -> Instant {
		self.at
	}

	/// The payload job in which this stage was reached.
	pub const fn payload_id(&self) -> PayloadId {
		self.payload_id
	}
}

/// The moment a transaction was mined in a canonical block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxMined {
	at: Instant,
	block_number: u64,
	block_timestamp: u64,
}

impl TxMined {
	/// When the block containing the transaction was committed to the chain.
	pub const fn at(&self) -> Instant {
		self.at
	}

	/// The number of the block that contains the transaction.
	pub const fn block_number(&self) -> u64 {
		self.block_number
	}

	/// The timestamp of the block header that contains the transaction.
	pub const fn block_timestamp(&self) -> u64 {
		self.block_timestamp
	}
}

/// The arrival of a transaction: when and where the pool saw it first.
#[derive(Debug, Clone, Copy)]
struct Received {
	at: Instant,
	wall: SystemTime,
	source: TxSource,
}

impl Received {
	fn new(source: TxSource, at: Instant) -> Self {
		let wall = SystemTime::now()
			.checked_sub(at.elapsed())
			.unwrap_or_else(SystemTime::now);

		Self { at, wall, source }
	}
}

/// The lifecycle of a single transaction as observed by the order pool.
///
/// Durations between stages are measured with monotonic [`Instant`]s. The
/// wall clock time of arrival is kept separately in [`Self::received_at`] for
/// correlation with external systems. The arrival is unknown for provisional
/// entries, which are created when a transaction is considered or included
/// before its arrival was reported; the deltas of such an entry are `None`.
///
/// # Stages
///
/// `received? -> considered -> included -> mined | dropped`, with these
/// edges:
///  - considered and included may come before received (a provisional entry);
///    every stage keeps its first moment, considered also keeps its last one;
///  - mined and dropped never coexist: a mined transaction is not dropped, and
///    mining a dropped transaction (for example a member orphaned by an evicted
///    bundle that lands from the mempool) clears the drop;
///  - revert edge: a chain reorg that reverts the block clears mined and
///    increments [`Self::reorg_count`], the transaction can be mined again;
///  - revive edge: a dropped transaction that is received again (re-submitted
///    in a new bundle, or accepted by the mempool) is no longer dropped; its
///    arrival stays the first sighting.
#[derive(Debug, Clone)]
pub struct TxLifecycle {
	received: Option<Received>,
	first_considered: Option<TxPayloadStage>,
	last_considered: Option<Instant>,
	first_included: Option<TxPayloadStage>,
	mined: Option<TxMined>,
	dropped: Option<Instant>,
	reorgs: u32,
	last_activity: Instant,
}

/// Accessors
impl TxLifecycle {
	/// Where the transaction was first seen, if its arrival was observed.
	pub fn source(&self) -> Option<TxSource> {
		self.received.map(|received| received.source)
	}

	/// When the transaction was received, if its arrival was observed.
	pub fn received(&self) -> Option<Instant> {
		self.received.map(|received| received.at)
	}

	/// The wall clock time at which the transaction was received, if its
	/// arrival was observed.
	pub fn received_at(&self) -> Option<SystemTime> {
		self.received.map(|received| received.wall)
	}

	/// The first time the transaction was considered for inclusion in a
	/// payload, if it was ever considered.
	pub const fn first_considered(&self) -> Option<TxPayloadStage> {
		self.first_considered
	}

	/// The last time the transaction was considered for inclusion in a
	/// payload, if it was ever considered.
	pub const fn last_considered(&self) -> Option<Instant> {
		self.last_considered
	}

	/// The first time the transaction was included in a built payload, if it
	/// was ever included.
	pub const fn first_included(&self) -> Option<TxPayloadStage> {
		self.first_included
	}

	/// The canonical block that contains the transaction, if it is mined. A
	/// chain reorg that reverts that block clears this stage again.
	pub const fn mined(&self) -> Option<TxMined> {
		self.mined
	}

	/// When the transaction was dropped from the pool without being mined, if
	/// it was dropped.
	pub const fn dropped(&self) -> Option<Instant> {
		self.dropped
	}

	/// How many times a block containing this transaction was reverted by a
	/// chain reorg after the transaction was recorded as mined.
	pub const fn reorg_count(&self) -> u32 {
		self.reorgs
	}

	/// Time between arrival and the first inclusion attempt, if both are known.
	pub fn time_to_first_consideration(&self) -> Option<Duration> {
		self
			.first_considered
			.and_then(|stage| self.since_received(stage.at))
	}

	/// Time between arrival and the first inclusion in a built payload, if both
	/// are known.
	pub fn time_to_inclusion(&self) -> Option<Duration> {
		self
			.first_included
			.and_then(|stage| self.since_received(stage.at))
	}

	/// Time between arrival and the commitment of the block that contains the
	/// transaction, if both are known.
	pub fn time_to_mined(&self) -> Option<Duration> {
		self.mined.and_then(|mined| self.since_received(mined.at))
	}

	/// The most recent moment at which any stage of this lifecycle was
	/// recorded. Used to decide when an entry became stale.
	pub const fn last_activity(&self) -> Instant {
		self.last_activity
	}

	fn since_received(&self, at: Instant) -> Option<Duration> {
		self
			.received
			.map(|received| at.saturating_duration_since(received.at))
	}

	fn since_received_ms(&self, at: Instant) -> Option<u128> {
		self.since_received(at).map(|elapsed| elapsed.as_millis())
	}

	fn received_at_unix_ms(&self) -> Option<u128> {
		self
			.received_at()
			.and_then(|wall| wall.duration_since(UNIX_EPOCH).ok())
			.map(|since_epoch| since_epoch.as_millis())
	}
}

/// Transitions. Every transition returns `true` when it changed the entry in
/// a way that is worth a log line, so that the caller can log after the map
/// guard that protects the entry is released.
impl TxLifecycle {
	/// An entry without any stage, created at the given moment.
	const fn empty(at: Instant) -> Self {
		Self {
			received: None,
			first_considered: None,
			last_considered: None,
			first_included: None,
			mined: None,
			dropped: None,
			reorgs: 0,
			last_activity: at,
		}
	}

	fn touch(&mut self, at: Instant) {
		self.last_activity = self.last_activity.max(at);
	}

	/// The first sighting wins.
	fn arrive(&mut self, source: TxSource, at: Instant) -> bool {
		let first_sighting = self.received.is_none();
		if first_sighting {
			self.received = Some(Received::new(source, at));
			self.touch(at);
		}
		first_sighting
	}

	/// A dropped transaction that is received again is no longer dropped (the
	/// revive edge). A no-op for transactions that are not dropped.
	fn revive(&mut self, at: Instant) -> bool {
		let revived = self.dropped.take().is_some();
		if revived {
			self.touch(at);
		}
		revived
	}

	/// The first consideration is kept, the last one is refreshed every time.
	fn consider(&mut self, payload_id: PayloadId, at: Instant) -> bool {
		self.last_considered = Some(at);
		self.touch(at);
		let first_consideration = self.first_considered.is_none();
		if first_consideration {
			self.first_considered = Some(TxPayloadStage { at, payload_id });
		}
		first_consideration
	}

	/// The first inclusion wins.
	fn include(&mut self, payload_id: PayloadId, at: Instant) -> bool {
		let first_inclusion = self.first_included.is_none();
		if first_inclusion {
			self.first_included = Some(TxPayloadStage { at, payload_id });
			self.touch(at);
		}
		first_inclusion
	}

	/// The first block wins until it is reverted. A mined transaction is not
	/// dropped, so an earlier drop is cleared.
	fn mine(
		&mut self,
		block_number: u64,
		block_timestamp: u64,
		at: Instant,
	) -> bool {
		let not_yet_mined = self.mined.is_none();
		if not_yet_mined {
			self.mined = Some(TxMined {
				at,
				block_number,
				block_timestamp,
			});
			self.dropped = None;
			self.touch(at);
		}
		not_yet_mined
	}

	/// Mined transactions are never dropped, and a drop is recorded once.
	fn drop_from_pool(&mut self, at: Instant) -> bool {
		let still_pending = self.mined.is_none() && self.dropped.is_none();
		if still_pending {
			self.dropped = Some(at);
			self.touch(at);
		}
		still_pending
	}

	/// Clears the mined stage after the block that contained the transaction
	/// was reverted by a chain reorg. A no-op for transactions that are not
	/// mined.
	fn revert(&mut self, at: Instant) -> bool {
		let was_mined = self.mined.is_some();
		if was_mined {
			self.mined = None;
			self.reorgs = self.reorgs.saturating_add(1);
			self.touch(at);
		}
		was_mined
	}
}

/// The member transactions of an order, remembered so that pipeline events for
/// an order can be resolved to its transactions after the order left the pool.
/// `active` is the last moment the record was written or resolved; records
/// are pruned by the retention relative to it.
#[derive(Debug, Clone)]
struct OrderRecord {
	members: Vec<TxHash>,
	active: Instant,
}

/// The state shared by all clones of a [`TxLifecycleLog`].
#[derive(Debug, Default)]
struct Shared {
	entries: DashMap<TxHash, TxLifecycle>,
	orders: DashMap<B256, OrderRecord>,
	refused: AtomicU64,
	refused_orders: AtomicU64,
	last_prune: Mutex<Option<Instant>>,
}

/// Whether a stage may create the entry of a transaction that the log does not
/// know yet.
#[derive(Debug, Clone, Copy)]
enum Create {
	/// Create a provisional entry, subject to the capacity guard.
	IfUnknown,
	/// Unknown transactions are ignored.
	Never,
}

/// What the capacity guard admits into the log: selects the bounded map, the
/// counter of refusals and the wording of the capacity line.
#[derive(Debug, Clone, Copy)]
enum Admission {
	/// The lifecycle entry of a transaction, keyed by its hash.
	Transaction,
	/// The member record of a bundle, keyed by the bundle hash.
	Order,
}

impl Admission {
	/// The number of items in the map this admission goes into.
	fn len(self, shared: &Shared) -> usize {
		match self {
			Self::Transaction => shared.entries.len(),
			Self::Order => shared.orders.len(),
		}
	}

	/// Whether the map this admission goes into holds `hash` already.
	fn contains(self, shared: &Shared, hash: &B256) -> bool {
		match self {
			Self::Transaction => shared.entries.contains_key(hash),
			Self::Order => shared.orders.contains_key(hash),
		}
	}

	/// The counter of admissions of this kind that were refused.
	fn refused(self, shared: &Shared) -> &AtomicU64 {
		match self {
			Self::Transaction => &shared.refused,
			Self::Order => &shared.refused_orders,
		}
	}

	/// The message of the capacity line emitted for a refused admission.
	const fn refusal_message(self) -> &'static str {
		match self {
			Self::Transaction => {
				"lifecycle log is at capacity, transaction is not tracked"
			}
			Self::Order => "lifecycle log is at capacity, order is not remembered",
		}
	}
}

/// A bounded, thread-safe log of transaction lifecycles.
///
/// Notes:
///  - This type is cheap to clone, all clones of this type share the same
///    underlying log.
///  - Every `record_*` method takes the moment of the event explicitly so that
///    callers can stamp a batch of events with one reading of the clock and so
///    that tests are deterministic ([`Self::record_dropped_now`] reads the
///    clock for the one caller that has no batch to stamp).
///  - Received, considered and included stages create the entry of a
///    transaction the log does not know yet. Mined, dropped and reverted stages
///    are no-ops for unknown transactions.
///  - Bounds: the log is a bounded window over recent transactions. It keeps at
///    most [`Self::capacity`] entries and forgets an entry once it had no
///    activity for [`Self::retention`], so the sustained admission rate it can
///    track is `capacity / retention`, about 2000 transactions per second at
///    the defaults (see [`Self::ASSUMED_ARRIVAL_RATE_PER_SEC`]). A transaction
///    with no activity for longer than the retention is forgotten and its mined
///    line is not emitted; for a longer window raise the retention and the
///    capacity together so that their ratio stays above the arrival rate.
///    Entries that are considered every block stay alive by design; that can
///    only pin as many entries as the pipeline considers per block, far below
///    the capacity. The same capacity bounds the remembered bundle records.
///  - Memory, entries: one entry is `size_of::<TxLifecycle>()` (about 210
///    bytes) plus a 32 byte key plus the hash table overhead, roughly 300 bytes
///    in total, so the default capacity bounds the entries at about 75 MB.
///  - Memory, bundle records: one record is a 32 byte key plus about 40 bytes
///    (a `Vec` header and an `Instant`) plus 32 bytes per member, before the
///    hash table overhead, so the records take at most `capacity x (~72 B + 32
///    B per member)`; in practice far fewer, since only bundles are recorded (a
///    transaction order needs no record) and a bundle holds a few transactions.
#[derive(Debug, Clone)]
pub struct TxLifecycleLog {
	shared: Arc<Shared>,
	retention: Duration,
	capacity: usize,
}

impl Default for TxLifecycleLog {
	fn default() -> Self {
		Self {
			shared: Arc::default(),
			retention: Self::DEFAULT_RETENTION,
			capacity: Self::DEFAULT_CAPACITY,
		}
	}
}

/// Configuration
impl TxLifecycleLog {
	/// The sustained transaction arrival rate, per second, that the default
	/// capacity and retention are sized for. `DEFAULT_CAPACITY` covers this
	/// rate over `DEFAULT_RETENTION` (2000 * 120 = 240 000 <= 250 000); a
	/// higher sustained rate fills the log before entries become stale and new
	/// transactions are refused until they do.
	pub const ASSUMED_ARRIVAL_RATE_PER_SEC: u64 = 2_000;
	/// The maximum number of tracked transactions by default. Sized for
	/// [`Self::ASSUMED_ARRIVAL_RATE_PER_SEC`] over [`Self::DEFAULT_RETENTION`],
	/// see the sizing and memory notes on the type.
	pub const DEFAULT_CAPACITY: usize = 250_000;
	/// Entries whose last activity is older than this are pruned by default.
	pub const DEFAULT_RETENTION: Duration = Duration::from_mins(2);

	/// Sets how long an entry is kept after its last recorded activity.
	#[must_use]
	pub fn with_retention(mut self, retention: Duration) -> Self {
		self.retention = retention;
		self
	}

	/// Sets the maximum number of tracked transactions and of remembered order
	/// records. Once the log is full, a stale entry prune runs (at most once
	/// per tenth of the retention) before a new transaction or order is
	/// tracked, and if the log is still full the transaction is refused and
	/// counted in [`Self::refused`] (orders in [`Self::refused_orders`]). The
	/// bound is not enforced strictly under concurrent inserts.
	#[must_use]
	pub fn with_capacity(mut self, capacity: usize) -> Self {
		self.capacity = capacity;
		self
	}

	/// How long an entry is kept after its last recorded activity.
	pub const fn retention(&self) -> Duration {
		self.retention
	}

	/// The maximum number of tracked transactions.
	pub const fn capacity(&self) -> usize {
		self.capacity
	}
}

/// Queries
impl TxLifecycleLog {
	/// Returns a snapshot of the lifecycle of the given transaction, if it is
	/// tracked.
	pub fn get(&self, hash: &TxHash) -> Option<TxLifecycle> {
		self
			.shared
			.entries
			.get(hash)
			.map(|entry| entry.value().clone())
	}

	/// The number of tracked transactions.
	pub fn len(&self) -> usize {
		self.shared.entries.len()
	}

	/// Returns `true` if no transactions are tracked.
	pub fn is_empty(&self) -> bool {
		self.shared.entries.is_empty()
	}

	/// The number of transactions that were not tracked because the log was
	/// full.
	pub fn refused(&self) -> u64 {
		self.shared.refused.load(Ordering::Relaxed)
	}

	/// The number of order records that were not remembered because the log
	/// was full.
	pub fn refused_orders(&self) -> u64 {
		self.shared.refused_orders.load(Ordering::Relaxed)
	}

	/// The member transactions of an order recorded through
	/// [`Self::record_order`], if the order is still remembered. Resolving an
	/// order counts as activity of its record: the record stays remembered for
	/// the retention after `now`, so a long-lived order that keeps producing
	/// events keeps its record.
	pub fn members_of(
		&self,
		order_hash: &B256,
		now: Instant,
	) -> Option<Vec<TxHash>> {
		// the guard is released at the end of the closure, before any other map
		// call can happen.
		self.shared.orders.get_mut(order_hash).map(|mut record| {
			record.active = record.active.max(now);
			record.members.clone()
		})
	}

	/// A snapshot of every tracked transaction and its lifecycle, in no
	/// particular order, for building metrics or exports without parsing the
	/// log lines. This clones every entry and is O(n) in the number of tracked
	/// transactions.
	pub fn snapshot(&self) -> Vec<(TxHash, TxLifecycle)> {
		self
			.shared
			.entries
			.iter()
			.map(|entry| (*entry.key(), entry.value().clone()))
			.collect()
	}
}

/// Recording
impl TxLifecycleLog {
	/// Records that a transaction was received at the given moment. The first
	/// sighting of a transaction wins, later calls for the same hash keep the
	/// recorded arrival and source. Fills in the arrival of a provisional entry,
	/// and revives a dropped transaction: it is no longer dropped.
	pub fn record_received(
		&self,
		hash: TxHash,
		source: TxSource,
		received: Instant,
	) {
		let mut revived = false;
		let arrived = self.transition(hash, received, Create::IfUnknown, |entry| {
			revived = entry.revive(received);
			entry.arrive(source, received) || revived
		});

		if let Some(entry) = arrived {
			debug!(
				target: TARGET,
				%hash,
				?source,
				revived,
				considered_before_received = entry.first_considered.is_some(),
				"transaction received"
			);
		}
	}

	/// Remembers the member transactions of an order so that pipeline events
	/// keyed by the order hash can be resolved to its transactions even after
	/// the order was evicted from the pool. Records are forgotten by
	/// [`Self::prune`] once they were neither written nor resolved (see
	/// [`Self::members_of`]) for the retention. The records are bounded by the
	/// same capacity as the transactions: a new record for a full log is only
	/// remembered after a prune made room for it, otherwise it is refused and
	/// counted in [`Self::refused_orders`]. Recording a remembered order again
	/// replaces its record.
	pub fn record_order(
		&self,
		order_hash: B256,
		members: Vec<TxHash>,
		recorded: Instant,
	) {
		if self.has_room_for(Admission::Order, &order_hash, recorded) {
			self.shared.orders.insert(order_hash, OrderRecord {
				members,
				active: recorded,
			});
		}
	}

	/// Records that a transaction was considered for inclusion in the given
	/// payload job at the given moment. Creates a provisional entry for an
	/// unknown transaction.
	pub fn record_considered(
		&self,
		hash: TxHash,
		payload_id: PayloadId,
		at: Instant,
	) {
		let first_consideration =
			self.transition(hash, at, Create::IfUnknown, |entry| {
				entry.consider(payload_id, at)
			});

		if let Some(entry) = first_consideration {
			debug!(
				target: TARGET,
				%hash,
				%payload_id,
				source = ?entry.source(),
				since_received_ms = entry.since_received_ms(at),
				"transaction considered for inclusion"
			);
		}
	}

	/// Records that a transaction was included in the payload built by the
	/// given payload job at the given moment. Creates a provisional entry for
	/// an unknown transaction.
	pub fn record_included(
		&self,
		hash: TxHash,
		payload_id: PayloadId,
		at: Instant,
	) {
		let first_inclusion =
			self.transition(hash, at, Create::IfUnknown, |entry| {
				entry.include(payload_id, at)
			});

		if let Some(entry) = first_inclusion {
			debug!(
				target: TARGET,
				%hash,
				%payload_id,
				source = ?entry.source(),
				since_received_ms = entry.since_received_ms(at),
				"transaction included in payload"
			);
		}
	}

	/// Records that a transaction was mined in the given canonical block that
	/// was committed at the given moment. The first block wins until it is
	/// reverted, unknown transactions are ignored.
	pub fn record_mined(
		&self,
		hash: TxHash,
		block_number: u64,
		block_timestamp: u64,
		at: Instant,
	) {
		let mined = self.transition(hash, at, Create::Never, |entry| {
			entry.mine(block_number, block_timestamp, at)
		});

		if let Some(entry) = mined {
			info!(
				target: TARGET,
				%hash,
				block_number,
				block_timestamp,
				source = ?entry.source(),
				received_at_unix_ms = entry.received_at_unix_ms(),
				to_first_consideration_ms = entry
					.time_to_first_consideration()
					.map(|elapsed| elapsed.as_millis()),
				to_inclusion_ms = entry
					.time_to_inclusion()
					.map(|elapsed| elapsed.as_millis()),
				to_mined_ms = entry.since_received_ms(at),
				reorgs = (entry.reorgs > 0).then_some(entry.reorgs),
				"transaction mined"
			);
		}
	}

	/// Records that the given block, which contained the transaction, was
	/// reverted by a chain reorg at the given moment: the mined stage is
	/// cleared and the transaction can be mined again in the new chain. A no-op
	/// for transactions that are not mined and for unknown transactions.
	pub fn record_reverted(&self, hash: TxHash, block_number: u64, at: Instant) {
		let reverted =
			self.transition(hash, at, Create::Never, |entry| entry.revert(at));

		if let Some(entry) = reverted {
			debug!(
				target: TARGET,
				%hash,
				block_number,
				reorgs = entry.reorgs,
				"block containing the transaction was reverted"
			);
		}
	}

	/// Records that a transaction was dropped from the pool at the given
	/// moment. Mined transactions are never marked as dropped, unknown
	/// transactions are ignored.
	pub fn record_dropped(&self, hash: TxHash, at: Instant) {
		let dropped = self
			.transition(hash, at, Create::Never, |entry| entry.drop_from_pool(at));

		if let Some(entry) = dropped {
			debug!(
				target: TARGET,
				%hash,
				source = ?entry.source(),
				since_received_ms = entry.since_received_ms(at),
				"transaction dropped from the pool"
			);
		}
	}

	/// Records that a transaction was dropped from the pool now. Used where a
	/// single transaction leaves the pool outside of any batch of events.
	pub fn record_dropped_now(&self, hash: TxHash) {
		self.record_dropped(hash, Instant::now());
	}

	/// Records that the last order holding a transaction was evicted from the
	/// pool at the given moment. Like [`Self::record_dropped`], except that a
	/// transaction received from the host node mempool is not marked as
	/// dropped: it still lives in the transaction pool of the host node, which
	/// can mine it independently of any order.
	pub fn record_dropped_unless_mempool(&self, hash: TxHash, at: Instant) {
		let dropped = self.transition(hash, at, Create::Never, |entry| {
			entry.source() != Some(TxSource::Mempool) && entry.drop_from_pool(at)
		});

		if let Some(entry) = dropped {
			debug!(
				target: TARGET,
				%hash,
				source = ?entry.source(),
				since_received_ms = entry.since_received_ms(at),
				"transaction dropped from the pool with its last order"
			);
		}
	}

	/// Removes all entries whose last activity is older than the retention
	/// period relative to `now`, and all order records that were neither
	/// written nor resolved within the retention. Returns the number of removed
	/// entries. This is the explicit, unconditional prune; the pool goes
	/// through the rate limited `prune_if_due` (at most once per tenth of the
	/// retention) on every committed block and when the log is full.
	pub fn prune(&self, now: Instant) -> usize {
		let entries = &self.shared.entries;
		let orders = &self.shared.orders;
		let retention = self.retention;

		let entries_before = entries.len();
		entries.retain(|_, lifecycle| {
			now.saturating_duration_since(lifecycle.last_activity()) <= retention
		});
		let pruned = entries_before.saturating_sub(entries.len());

		let orders_before = orders.len();
		orders.retain(|_, record| {
			now.saturating_duration_since(record.active) <= retention
		});
		let pruned_orders = orders_before.saturating_sub(orders.len());

		if pruned > 0 || pruned_orders > 0 {
			debug!(
				target: TARGET,
				pruned,
				pruned_orders,
				remaining = entries.len(),
				"pruned stale lifecycle entries"
			);
		}

		pruned
	}

	/// Runs [`Self::prune`] when the last prune is at least a tenth of the
	/// retention ago, so that a burst of committed blocks (sync, reorg) or of
	/// refused transactions does not run one prune after another. This is the
	/// single rate limited entry point used by the pool on every committed
	/// block and by the capacity guard. Returns the number of pruned entries
	/// when a prune ran.
	pub(super) fn prune_if_due(&self, now: Instant) -> Option<usize> {
		let interval = self.retention / 10;
		let due = {
			let mut last = self.shared.last_prune.lock();
			let due =
				last.is_none_or(|last| now.saturating_duration_since(last) >= interval);
			if due {
				*last = Some(now);
			}
			due
		};

		due.then(|| self.prune(now))
	}
}

/// Internals
impl TxLifecycleLog {
	/// Applies `transition` to the entry of `hash`, creating the entry first
	/// when `create` allows it and the log has room. Returns a snapshot of the
	/// entry when the transition reported a change worth logging. The map
	/// guard protecting the entry is released before this returns, so callers
	/// can log and call back into the log freely.
	///
	/// A tracked transaction takes the fast path: one shard lock and no
	/// capacity check. An unknown transaction goes through
	/// [`Self::transition_unknown`].
	fn transition(
		&self,
		hash: TxHash,
		at: Instant,
		create: Create,
		transition: impl FnOnce(&mut TxLifecycle) -> bool,
	) -> Option<TxLifecycle> {
		if let Some(mut entry) = self.shared.entries.get_mut(&hash) {
			transition(&mut entry).then(|| entry.value().clone())
		} else {
			self.transition_unknown(hash, at, create, transition)
		}
	}

	/// The slow path of [`Self::transition`] for a transaction that was not
	/// tracked a moment ago. A stage that never creates entries is done. Other
	/// stages run the capacity guard first, which may prune (and iterate the
	/// map), so it runs with no entry guard held; the entry is then taken
	/// through the `Entry` API, which handles another task creating it in the
	/// meantime.
	fn transition_unknown(
		&self,
		hash: TxHash,
		at: Instant,
		create: Create,
		transition: impl FnOnce(&mut TxLifecycle) -> bool,
	) -> Option<TxLifecycle> {
		match create {
			Create::Never => None,
			Create::IfUnknown => {
				let may_create = self.has_room_for(Admission::Transaction, &hash, at);

				match self.shared.entries.entry(hash) {
					Entry::Occupied(mut occupied) => {
						let entry = occupied.get_mut();
						transition(entry).then(|| entry.clone())
					}
					Entry::Vacant(vacant) => may_create
						.then(|| {
							let mut entry = vacant.insert(TxLifecycle::empty(at));
							transition(&mut entry).then(|| entry.value().clone())
						})
						.flatten(),
				}
			}
		}
	}

	/// Returns `true` if the map selected by `admission` can take `hash`: the
	/// map is below capacity or `hash` is in it already (another task inserted
	/// it), otherwise possibly after a prune. Called with no map guard held.
	fn has_room_for(
		&self,
		admission: Admission,
		hash: &B256,
		now: Instant,
	) -> bool {
		let shared = &self.shared;

		admission.len(shared) < self.capacity
			|| admission.contains(shared, hash)
			|| self.make_room(admission, hash, now)
	}

	/// The map selected by `admission` is full: prunes if due and checks the
	/// capacity again. A hash that is still refused afterwards is counted in
	/// the refusal counter of that admission.
	fn make_room(&self, admission: Admission, hash: &B256, now: Instant) -> bool {
		let pruned = self.prune_if_due(now);
		let room = admission.len(&self.shared) < self.capacity;

		if !room {
			let refused = admission
				.refused(&self.shared)
				.fetch_add(1, Ordering::Relaxed)
				.saturating_add(1);

			// the log line is rate limited by the same guard as the prune
			if pruned.is_some() {
				debug!(
					target: TARGET,
					%hash,
					capacity = self.capacity,
					refused,
					"{}",
					admission.refusal_message()
				);
			}
		}

		room
	}
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::test_utils::{test_bundle, test_txs},
		alloy::consensus::{BlockBody, Header},
		reth::primitives::SealedBlock,
		std::collections::BTreeSet,
	};

	const SECOND: Duration = Duration::from_secs(1);

	fn payload(id: u8) -> PayloadId {
		PayloadId::new([id, 0, 0, 0, 0, 0, 0, 0])
	}

	fn hash(seed: u8) -> TxHash {
		TxHash::repeat_byte(seed)
	}

	#[test]
	fn happy_path_records_all_stages_in_order() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		let tx = hash(1);

		log.record_received(tx, TxSource::Mempool, base);
		log.record_considered(tx, payload(1), base + SECOND);
		log.record_included(tx, payload(1), base + 2 * SECOND);
		log.record_mined(tx, 42, 1_700_000_000, base + 3 * SECOND);

		let lifecycle = log.get(&tx).expect("transaction is tracked");
		assert_eq!(lifecycle.source(), Some(TxSource::Mempool));
		assert_eq!(lifecycle.received(), Some(base));
		assert!(lifecycle.received_at().is_some());
		assert_eq!(lifecycle.time_to_first_consideration(), Some(SECOND));
		assert_eq!(lifecycle.time_to_inclusion(), Some(2 * SECOND));
		assert_eq!(lifecycle.time_to_mined(), Some(3 * SECOND));
		assert_eq!(lifecycle.dropped(), None);
		assert_eq!(lifecycle.reorg_count(), 0);

		let considered = lifecycle.first_considered().expect("considered");
		assert_eq!(considered.at(), base + SECOND);
		assert_eq!(considered.payload_id(), payload(1));

		let mined = lifecycle.mined().expect("mined");
		assert_eq!(mined.at(), base + 3 * SECOND);
		assert_eq!(mined.block_number(), 42);
		assert_eq!(mined.block_timestamp(), 1_700_000_000);
		assert_eq!(lifecycle.last_activity(), base + 3 * SECOND);
	}

	#[test]
	fn first_sighting_wins_and_later_stages_keep_the_first_moment() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		let tx = hash(2);

		log.record_received(tx, TxSource::Mempool, base);
		log.record_received(tx, TxSource::Bundle(hash(9)), base + SECOND);
		log.record_considered(tx, payload(1), base + SECOND);
		log.record_considered(tx, payload(2), base + 2 * SECOND);
		log.record_included(tx, payload(2), base + 2 * SECOND);
		log.record_included(tx, payload(3), base + 3 * SECOND);
		log.record_mined(tx, 1, 10, base + 4 * SECOND);
		log.record_mined(tx, 2, 20, base + 5 * SECOND);

		let lifecycle = log.get(&tx).expect("transaction is tracked");
		assert_eq!(lifecycle.source(), Some(TxSource::Mempool));
		assert_eq!(lifecycle.received(), Some(base));
		assert_eq!(
			lifecycle.first_considered().map(|stage| stage.payload_id()),
			Some(payload(1))
		);
		assert_eq!(lifecycle.last_considered(), Some(base + 2 * SECOND));
		assert_eq!(
			lifecycle.first_included().map(|stage| stage.payload_id()),
			Some(payload(2))
		);
		assert_eq!(lifecycle.mined().map(|mined| mined.block_number()), Some(1));
	}

	#[test]
	fn mined_dropped_and_reverted_never_create_entries() {
		let log = TxLifecycleLog::default();
		let now = Instant::now();
		let tx = hash(3);

		log.record_mined(tx, 1, 1, now);
		log.record_dropped(tx, now);
		log.record_dropped_unless_mempool(tx, now);
		log.record_reverted(tx, 1, now);

		assert!(log.get(&tx).is_none());
		assert!(log.is_empty());
		assert_eq!(log.len(), 0);
		assert_eq!(log.refused(), 0);
	}

	#[test]
	fn considered_before_received_creates_a_provisional_entry() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		let considered = hash(16);
		let included = hash(17);

		// the pipeline events listener runs ahead of the mempool listener
		log.record_considered(considered, payload(1), base + SECOND);
		let provisional = log.get(&considered).expect("provisional entry");
		assert_eq!(provisional.source(), None);
		assert_eq!(provisional.received(), None);
		assert_eq!(provisional.received_at(), None);
		assert_eq!(provisional.time_to_first_consideration(), None);
		assert_eq!(
			provisional
				.first_considered()
				.map(|stage| stage.payload_id()),
			Some(payload(1))
		);
		assert_eq!(provisional.last_activity(), base + SECOND);

		// the arrival is filled in later and the deltas become known
		log.record_received(considered, TxSource::Mempool, base);
		log.record_received(considered, TxSource::Transaction, base + 5 * SECOND);
		let filled = log.get(&considered).expect("tracked");
		assert_eq!(filled.source(), Some(TxSource::Mempool));
		assert_eq!(filled.received(), Some(base));
		assert_eq!(filled.time_to_first_consideration(), Some(SECOND));
		assert_eq!(filled.last_activity(), base + SECOND);

		// an inclusion also creates the entry
		log.record_included(included, payload(2), base + 2 * SECOND);
		let provisional = log.get(&included).expect("provisional entry");
		assert_eq!(provisional.source(), None);
		assert_eq!(provisional.time_to_inclusion(), None);
		assert_eq!(
			provisional.first_included().map(|stage| stage.payload_id()),
			Some(payload(2))
		);
		assert_eq!(log.len(), 2);
	}

	#[test]
	fn dropped_is_recorded_once_and_never_after_mined() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		let dropped = hash(4);
		let mined = hash(5);

		log.record_received(dropped, TxSource::Transaction, base);
		log.record_dropped(dropped, base + SECOND);
		log.record_dropped(dropped, base + 2 * SECOND);

		log.record_received(mined, TxSource::Transaction, base);
		log.record_mined(mined, 1, 1, base + SECOND);
		log.record_dropped(mined, base + 2 * SECOND);

		let dropped = log.get(&dropped).expect("tracked");
		assert_eq!(dropped.dropped(), Some(base + SECOND));
		assert_eq!(dropped.mined(), None);

		let mined = log.get(&mined).expect("tracked");
		assert_eq!(mined.dropped(), None);
		assert!(mined.mined().is_some());
	}

	#[test]
	fn mining_a_dropped_transaction_clears_the_drop() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		// a member orphaned by an evicted bundle that lands from the mempool
		let orphan = hash(24);
		// a dropped transaction that is never mined stays dropped
		let gone = hash(25);

		log.record_received(orphan, TxSource::Bundle(hash(99)), base);
		log.record_dropped(orphan, base + SECOND);
		assert_eq!(
			log.get(&orphan).and_then(|entry| entry.dropped()),
			Some(base + SECOND)
		);

		log.record_mined(orphan, 7, 70, base + 2 * SECOND);
		let mined = log.get(&orphan).expect("tracked");
		assert_eq!(mined.dropped(), None);
		assert_eq!(mined.mined().map(|mined| mined.block_number()), Some(7));
		assert_eq!(mined.last_activity(), base + 2 * SECOND);
		// and it cannot be dropped again while mined
		log.record_dropped(orphan, base + 3 * SECOND);
		assert_eq!(log.get(&orphan).and_then(|entry| entry.dropped()), None);

		log.record_received(gone, TxSource::Transaction, base);
		log.record_dropped(gone, base + SECOND);
		log.record_reverted(gone, 7, base + 2 * SECOND);
		let still_dropped = log.get(&gone).expect("tracked");
		assert_eq!(still_dropped.dropped(), Some(base + SECOND));
		assert_eq!(still_dropped.mined(), None);
	}

	#[test]
	fn receiving_a_dropped_transaction_again_revives_it() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		let resubmitted = hash(26);
		let pending = hash(27);

		// dropped with its bundle, then re-submitted in another bundle
		log.record_received(resubmitted, TxSource::Bundle(hash(98)), base);
		log.record_dropped(resubmitted, base + SECOND);
		log.record_received(
			resubmitted,
			TxSource::Bundle(hash(97)),
			base + 2 * SECOND,
		);

		let revived = log.get(&resubmitted).expect("tracked");
		assert_eq!(revived.dropped(), None);
		// the arrival stays the first sighting
		assert_eq!(revived.source(), Some(TxSource::Bundle(hash(98))));
		assert_eq!(revived.received(), Some(base));
		assert_eq!(revived.last_activity(), base + 2 * SECOND);
		assert_eq!(revived.mined(), None);

		// a transaction that was not dropped is unaffected by a second sighting
		log.record_received(pending, TxSource::Mempool, base);
		log.record_received(pending, TxSource::Bundle(hash(97)), base + 2 * SECOND);
		let unchanged = log.get(&pending).expect("tracked");
		assert_eq!(unchanged.dropped(), None);
		assert_eq!(unchanged.source(), Some(TxSource::Mempool));
		assert_eq!(unchanged.last_activity(), base);
	}

	#[test]
	fn dropped_unless_mempool_spares_mempool_transactions_only() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		let mempool = hash(18);
		let bundled = hash(19);
		let standalone = hash(20);

		log.record_received(mempool, TxSource::Mempool, base);
		log.record_received(bundled, TxSource::Bundle(hash(99)), base);
		log.record_received(standalone, TxSource::Transaction, base);

		log.record_dropped_unless_mempool(mempool, base + SECOND);
		log.record_dropped_unless_mempool(bundled, base + SECOND);
		log.record_dropped_unless_mempool(standalone, base + SECOND);

		assert_eq!(log.get(&mempool).and_then(|entry| entry.dropped()), None);
		assert_eq!(
			log.get(&bundled).and_then(|entry| entry.dropped()),
			Some(base + SECOND)
		);
		assert_eq!(
			log.get(&standalone).and_then(|entry| entry.dropped()),
			Some(base + SECOND)
		);

		// an explicit drop still applies to mempool transactions
		log.record_dropped(mempool, base + 2 * SECOND);
		assert_eq!(
			log.get(&mempool).and_then(|entry| entry.dropped()),
			Some(base + 2 * SECOND)
		);
	}

	#[test]
	fn reverted_block_clears_the_mined_stage_until_mined_again() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		let reorged = hash(21);
		let never_mined = hash(22);

		log.record_received(reorged, TxSource::Mempool, base);
		log.record_mined(reorged, 10, 100, base + SECOND);
		// duplicate commits of the same canonical block keep the first stamp
		log.record_mined(reorged, 10, 100, base + 2 * SECOND);
		assert_eq!(
			log.get(&reorged).and_then(|entry| entry.mined()),
			Some(TxMined {
				at: base + SECOND,
				block_number: 10,
				block_timestamp: 100,
			})
		);

		// the block is reverted by a reorg
		log.record_reverted(reorged, 10, base + 3 * SECOND);
		let unmined = log.get(&reorged).expect("tracked");
		assert_eq!(unmined.mined(), None);
		assert_eq!(unmined.reorg_count(), 1);
		assert_eq!(unmined.time_to_mined(), None);
		assert_eq!(unmined.last_activity(), base + 3 * SECOND);
		// a second revert of an unmined transaction changes nothing
		log.record_reverted(reorged, 10, base + 4 * SECOND);
		assert_eq!(log.get(&reorged).map(|entry| entry.reorg_count()), Some(1));

		// mined again in the new chain
		log.record_mined(reorged, 11, 110, base + 5 * SECOND);
		let remined = log.get(&reorged).expect("tracked");
		assert_eq!(remined.mined().map(|mined| mined.block_number()), Some(11));
		assert_eq!(remined.time_to_mined(), Some(5 * SECOND));
		assert_eq!(remined.reorg_count(), 1);
		assert_eq!(remined.dropped(), None);

		// a revert for a transaction that was never mined is a no-op
		log.record_received(never_mined, TxSource::Mempool, base);
		log.record_reverted(never_mined, 10, base + 3 * SECOND);
		let untouched = log.get(&never_mined).expect("tracked");
		assert_eq!(untouched.reorg_count(), 0);
		assert_eq!(untouched.last_activity(), base);
	}

	#[test]
	fn prune_removes_only_entries_older_than_retention() {
		let retention = Duration::from_mins(1);
		let log = TxLifecycleLog::default().with_retention(retention);
		let base = Instant::now();
		let now = base + 100 * SECOND;
		let recently = base + 99 * SECOND;
		let retention_edge = base + 40 * SECOND;

		// received long ago, no activity since: pruned
		let stale = hash(6);
		log.record_received(stale, TxSource::Mempool, base);

		// received long ago, but mined within the retention window: kept
		let mined_recently = hash(7);
		log.record_received(mined_recently, TxSource::Mempool, base);
		log.record_mined(mined_recently, 1, 1, recently);

		// received long ago, but still being considered: kept
		let still_considered = hash(8);
		log.record_received(still_considered, TxSource::Mempool, base);
		log.record_considered(still_considered, payload(1), base + SECOND);
		log.record_considered(still_considered, payload(2), recently);

		// received within the retention window: kept
		let fresh = hash(9);
		log.record_received(fresh, TxSource::Mempool, recently);

		// received exactly at the edge of the retention window: kept
		let edge = hash(10);
		log.record_received(edge, TxSource::Mempool, retention_edge);

		// order records follow the same retention
		let stale_order = hash(60);
		let fresh_order = hash(61);
		log.record_order(stale_order, vec![stale], base);
		log.record_order(fresh_order, vec![fresh], recently);

		assert_eq!(log.len(), 5);
		assert_eq!(log.prune(now), 1);
		assert_eq!(log.len(), 4);
		assert!(log.get(&stale).is_none());
		assert!(log.get(&mined_recently).is_some());
		assert!(log.get(&still_considered).is_some());
		assert!(log.get(&fresh).is_some());
		assert!(log.get(&edge).is_some());
		assert!(log.members_of(&stale_order, now).is_none());
		assert_eq!(log.members_of(&fresh_order, now), Some(vec![fresh]));

		// nothing else is stale yet
		assert_eq!(log.prune(now), 0);

		// once past retention everything goes
		assert_eq!(log.prune(now + retention + SECOND), 4);
		assert!(log.is_empty());
		assert!(log.members_of(&fresh_order, now).is_none());
	}

	#[test]
	fn capacity_refuses_new_transactions_until_pruned() {
		let log = TxLifecycleLog::default()
			.with_capacity(2)
			.with_retention(Duration::from_mins(1));
		let base = Instant::now();

		log.record_received(hash(11), TxSource::Mempool, base);
		log.record_received(hash(12), TxSource::Mempool, base);
		log.record_received(hash(13), TxSource::Mempool, base);

		assert_eq!(log.len(), 2);
		assert!(log.get(&hash(11)).is_some());
		assert!(log.get(&hash(12)).is_some());
		assert!(log.get(&hash(13)).is_none());
		assert_eq!(log.refused(), 1);

		// already tracked transactions are unaffected by the capacity guard
		log.record_received(hash(12), TxSource::Bundle(hash(99)), base);
		log.record_considered(hash(12), payload(1), base);
		assert_eq!(
			log.get(&hash(12)).map(|lifecycle| lifecycle.source()),
			Some(Some(TxSource::Mempool))
		);
		assert_eq!(log.refused(), 1);

		// provisional entries are subject to the same guard
		log.record_considered(hash(14), payload(1), base);
		assert!(log.get(&hash(14)).is_none());
		assert_eq!(log.refused(), 2);

		// stages that never create entries do not even reach the guard
		log.record_mined(hash(15), 1, 1, base);
		log.record_dropped(hash(15), base);
		assert!(log.get(&hash(15)).is_none());
		assert_eq!(log.refused(), 2);

		// pruning frees room for new transactions
		let later = base + 120 * SECOND;
		assert_eq!(log.prune(later), 2);
		log.record_received(hash(13), TxSource::Mempool, later);
		assert!(log.get(&hash(13)).is_some());
		assert_eq!(log.len(), 1);
		assert_eq!(log.refused(), 2);
	}

	#[test]
	fn full_log_prunes_on_demand_before_refusing() {
		let retention = Duration::from_mins(1);
		let log = TxLifecycleLog::default()
			.with_capacity(2)
			.with_retention(retention);
		let base = Instant::now();

		log.record_received(hash(11), TxSource::Mempool, base);
		log.record_received(hash(12), TxSource::Mempool, base);

		// full, nothing is stale: the on demand prune runs and finds nothing
		log.record_received(hash(13), TxSource::Mempool, base);
		assert!(log.get(&hash(13)).is_none());
		assert_eq!(log.refused(), 1);

		// still full, and the on demand prune is rate limited to once per
		// tenth of the retention (6 seconds here): refused without pruning
		log.record_received(hash(13), TxSource::Mempool, base + SECOND);
		assert!(log.get(&hash(13)).is_none());
		assert_eq!(log.refused(), 2);

		// once the tracked entries are stale a prune on demand makes room
		let later = base + retention + 10 * SECOND;
		log.record_received(hash(13), TxSource::Mempool, later);
		assert!(log.get(&hash(13)).is_some());
		assert!(log.get(&hash(11)).is_none());
		assert!(log.get(&hash(12)).is_none());
		assert_eq!(log.len(), 1);
		assert_eq!(log.refused(), 2);
	}

	#[test]
	fn default_capacity_covers_the_assumed_arrival_rate_over_the_retention() {
		let capacity =
			u64::try_from(TxLifecycleLog::DEFAULT_CAPACITY).expect("fits in u64");
		let needed = TxLifecycleLog::ASSUMED_ARRIVAL_RATE_PER_SEC
			* TxLifecycleLog::DEFAULT_RETENTION.as_secs();
		assert!(
			capacity >= needed,
			"capacity {capacity} does not cover {needed} arrivals over the retention"
		);
		// the defaults are what the docs say they are
		assert_eq!(TxLifecycleLog::default().capacity(), 250_000);
		assert_eq!(
			TxLifecycleLog::default().retention(),
			Duration::from_mins(2)
		);
	}

	#[test]
	fn order_records_are_bounded_by_the_capacity() {
		let log = TxLifecycleLog::default()
			.with_capacity(2)
			.with_retention(Duration::from_mins(1));
		let base = Instant::now();
		let (first, second, third) = (hash(70), hash(71), hash(72));

		log.record_order(first, vec![hash(1)], base);
		log.record_order(second, vec![hash(2)], base);
		// the records are full: a third order is refused and counted
		log.record_order(third, vec![hash(3)], base);
		assert_eq!(log.members_of(&third, base), None);
		assert_eq!(log.refused_orders(), 1);
		assert_eq!(log.refused(), 0);

		// a remembered order can be recorded again while the records are full
		log.record_order(second, vec![hash(2), hash(4)], base + SECOND);
		assert_eq!(log.members_of(&second, base), Some(vec![hash(2), hash(4)]));
		assert_eq!(log.refused_orders(), 1);

		// once the records are stale a prune makes room
		let later = base + 2 * Duration::from_mins(1);
		log.record_order(third, vec![hash(3)], later);
		assert_eq!(log.members_of(&third, later), Some(vec![hash(3)]));
		assert_eq!(log.members_of(&first, later), None);
		assert_eq!(log.refused_orders(), 1);
	}

	#[test]
	fn resolving_an_order_keeps_its_record_alive() {
		let retention = Duration::from_mins(1);
		let log = TxLifecycleLog::default().with_retention(retention);
		let base = Instant::now();
		let (resolved, idle) = (hash(73), hash(74));

		log.record_order(resolved, vec![hash(5)], base);
		log.record_order(idle, vec![hash(6)], base);

		// resolving the order 50 s in counts as activity of its record
		assert_eq!(
			log.members_of(&resolved, base + 50 * SECOND),
			Some(vec![hash(5)])
		);

		// 70 s in both records are past the retention of their insertion, only
		// the resolved one survives
		log.prune(base + 70 * SECOND);
		assert_eq!(
			log.members_of(&resolved, base + 70 * SECOND),
			Some(vec![hash(5)])
		);
		assert_eq!(log.members_of(&idle, base + 70 * SECOND), None);

		// and it goes once the retention after its last resolution has passed
		log.prune(base + 70 * SECOND + retention + SECOND);
		assert_eq!(log.members_of(&resolved, base + 132 * SECOND), None);
	}

	#[test]
	fn snapshot_lists_every_tracked_transaction() {
		let log = TxLifecycleLog::default();
		let base = Instant::now();
		assert!(log.snapshot().is_empty());

		log.record_received(hash(28), TxSource::Mempool, base);
		log.record_considered(hash(29), payload(1), base + SECOND);
		log.record_mined(hash(30), 1, 1, base);

		let hashes: BTreeSet<TxHash> = log
			.snapshot()
			.into_iter()
			.map(|(hash, lifecycle)| {
				assert_eq!(lifecycle.mined(), None);
				hash
			})
			.collect();
		assert_eq!(hashes, BTreeSet::from([hash(28), hash(29)]));
	}

	#[test]
	fn lifecycle_entry_stays_within_the_documented_envelope() {
		// the memory note on TxLifecycleLog assumes about 210 bytes per entry
		// and about 40 bytes per bundle record before its members; a future
		// field must not silently blow either envelope up.
		let size = size_of::<TxLifecycle>();
		assert!(size <= 256, "TxLifecycle is {size} bytes");
		let record = size_of::<OrderRecord>();
		assert!(record <= 48, "OrderRecord is {record} bytes");
	}

	#[test]
	fn clones_share_the_same_log() {
		let log = TxLifecycleLog::default();
		let clone = log.clone();
		let now = Instant::now();

		log.record_received(hash(14), TxSource::Mempool, now);
		log.record_order(hash(15), vec![hash(14)], now);
		assert!(clone.get(&hash(14)).is_some());
		assert_eq!(clone.len(), 1);
		assert_eq!(clone.members_of(&hash(15), now), Some(vec![hash(14)]));
	}

	/// Snapshots the lifecycle of every given transaction, all must be tracked.
	fn lifecycles<P: Platform>(
		pool: &OrderPool<P>,
		hashes: &[TxHash],
	) -> Vec<TxLifecycle> {
		hashes
			.iter()
			.map(|tx| pool.lifecycle().get(tx).expect("member is tracked"))
			.collect()
	}

	/// Whether each of the given transactions is marked as dropped, all must
	/// be tracked.
	fn dropped_flags<P: Platform>(
		pool: &OrderPool<P>,
		hashes: &[TxHash],
	) -> Vec<bool> {
		lifecycles(pool, hashes)
			.iter()
			.map(|lifecycle| lifecycle.dropped().is_some())
			.collect()
	}

	/// Three transactions of the given funded account and their hashes.
	fn three_txs(
		key: u32,
	) -> (Vec<Recovered<types::Transaction<Ethereum>>>, [TxHash; 3]) {
		let txs = test_txs::<Ethereum>(key, 0, 3);
		let hashes: Vec<TxHash> = txs.iter().map(|tx| *tx.tx_hash()).collect();
		let hashes: [TxHash; 3] = hashes.try_into().expect("three transactions");
		(txs, hashes)
	}

	/// A bundle made of the given transactions, valid for the given block
	/// (0 means any block).
	fn bundle_of<'a>(
		txs: impl IntoIterator<Item = &'a Recovered<types::Transaction<Ethereum>>>,
		block_number: u64,
	) -> types::Bundle<Ethereum> {
		txs
			.into_iter()
			.cloned()
			.fold(FlashbotsBundle::<Ethereum>::default(), |bundle, tx| {
				bundle.with_transaction(tx)
			})
			.with_block_number(block_number)
	}

	#[test]
	fn bundle_members_are_recorded_through_the_order_pool() {
		let pool = OrderPool::<Ethereum>::default();
		let (bundle, txs) = test_bundle::<Ethereum>(0, 0);
		let bundle_hash = bundle.hash();
		let hashes: Vec<TxHash> = txs.iter().map(|tx| *tx.tx_hash()).collect();
		assert_eq!(hashes.len(), 3);

		pool.insert(Order::Bundle(bundle));

		let received = lifecycles(&pool, &hashes);
		assert!(received.iter().all(|lifecycle| {
			lifecycle.source() == Some(TxSource::Bundle(bundle_hash))
				&& lifecycle.first_considered().is_none()
		}));
		assert_eq!(
			pool.lifecycle().members_of(&bundle_hash, Instant::now()),
			Some(hashes.clone())
		);

		// events are keyed by order hash and resolved to every member
		pool.report_inclusion_attempt(bundle_hash, payload(1));
		pool.report_inclusion_success(bundle_hash, payload(1));

		let attempted = lifecycles(&pool, &hashes);
		assert!(attempted.iter().all(|lifecycle| {
			lifecycle.first_considered().map(|stage| stage.payload_id())
				== Some(payload(1))
				&& lifecycle.first_included().map(|stage| stage.payload_id())
					== Some(payload(1))
				&& lifecycle.dropped().is_none()
		}));

		// a permanent execution error drops every member of the order
		pool.report_execution_error(
			bundle_hash,
			&ExecutionError::IneligibleBundle(Eligibility::PermanentlyIneligible),
		);

		assert_eq!(dropped_flags(&pool, &hashes), vec![true, true, true]);
		assert!(pool.inner.orders.get(&bundle_hash).is_none());
	}

	#[test]
	fn late_events_for_an_evicted_bundle_still_reach_its_members() {
		let pool = OrderPool::<Ethereum>::default();
		let (bundle, txs) = test_bundle::<Ethereum>(1, 0);
		let bundle_hash = bundle.hash();
		let hashes: Vec<TxHash> = txs.iter().map(|tx| *tx.tx_hash()).collect();

		pool.insert(Order::Bundle(bundle));
		pool.remove(&bundle_hash);

		// the bundle is gone from the pool but its members are still known
		pool.report_inclusion_attempt(bundle_hash, payload(1));
		pool.report_inclusion_success(bundle_hash, payload(1));

		let members = lifecycles(&pool, &hashes);
		assert!(members.iter().all(|lifecycle| {
			lifecycle.first_considered().is_some()
				&& lifecycle.first_included().is_some()
		}));
		// the bundle hash itself was not mistaken for a transaction hash
		assert!(pool.lifecycle().get(&bundle_hash).is_none());
	}

	#[test]
	fn removing_an_order_drops_only_the_members_no_live_order_holds() {
		let pool = OrderPool::<Ethereum>::default();
		let (txs, [x, y, z]) = three_txs(2);
		// A = {X, Y}, B = {X, Z}
		let bundle_a = bundle_of(txs.iter().take(2), 0);
		let bundle_b = bundle_of(txs.iter().step_by(2), 0);
		let (a_hash, b_hash) = (bundle_a.hash(), bundle_b.hash());
		assert_ne!(a_hash, b_hash);

		pool.insert(Order::Bundle(bundle_a));
		pool.insert(Order::Bundle(bundle_b));
		assert_eq!(dropped_flags(&pool, &[x, y, z]), vec![false, false, false]);

		// removing A orphans Y only, X and Z are still held by B
		pool.remove(&a_hash);
		assert_eq!(dropped_flags(&pool, &[x, y, z]), vec![false, true, false]);
		assert!(pool.inner.tx_map.get(&y).is_none(), "Y left the tx map");
		assert!(
			pool.inner.tx_map.get(&x).is_some(),
			"X is still mapped to B"
		);

		// removing B orphans X and Z
		pool.remove(&b_hash);
		assert_eq!(dropped_flags(&pool, &[x, y, z]), vec![true, true, true]);
		assert!(pool.inner.tx_map.is_empty());

		// removing an unknown order changes nothing
		pool.remove(&hash(23));
		assert_eq!(dropped_flags(&pool, &[x, y, z]), vec![true, true, true]);
	}

	#[test]
	fn removing_an_order_never_drops_mempool_transactions() {
		let pool = OrderPool::<Ethereum>::default();
		let (txs, [x, y, _z]) = three_txs(3);
		let bundle_a = bundle_of(txs.iter().take(2), 0);
		let a_hash = bundle_a.hash();

		// X arrived through the mempool before it showed up in bundle A
		pool.report_mempool_transaction(x, Instant::now());
		pool.insert(Order::Bundle(bundle_a));
		assert_eq!(
			pool.lifecycle().get(&x).and_then(|entry| entry.source()),
			Some(TxSource::Mempool)
		);

		pool.remove(&a_hash);
		// X still lives in the mempool, Y only lived in bundle A
		assert_eq!(dropped_flags(&pool, &[x, y]), vec![false, true]);
	}

	#[test]
	fn removing_any_with_drops_the_transaction_and_its_orphaned_siblings() {
		let pool = OrderPool::<Ethereum>::default();
		let (txs, [x, y, z]) = three_txs(4);
		// A = {X, Y}, B = {Y, Z}
		let bundle_a = bundle_of(txs.iter().take(2), 0);
		let bundle_b = bundle_of(txs.iter().skip(1), 0);
		let b_hash = bundle_b.hash();

		pool.insert(Order::Bundle(bundle_a));
		pool.insert(Order::Bundle(bundle_b));

		// X leaves the pool: it is dropped explicitly and A goes with it, but
		// A's other member Y is still held by B
		pool.remove_any_with(x);
		assert_eq!(dropped_flags(&pool, &[x, y, z]), vec![true, false, false]);
		assert!(pool.inner.orders.get(&b_hash).is_some());

		// Y leaves the pool: B goes with it and orphans Z
		pool.remove_any_with(y);
		assert_eq!(dropped_flags(&pool, &[x, y, z]), vec![true, true, true]);
		assert!(pool.inner.orders.is_empty());
		assert!(pool.inner.tx_map.is_empty());
	}

	#[test]
	fn invalidated_orders_drop_their_members_when_removed() {
		let pool = OrderPool::<Ethereum>::default();
		let (txs, [x, y, _z]) = three_txs(5);
		// only valid for block 1: permanently ineligible once block 2 is the tip
		let expired = bundle_of(txs.iter().take(1), 1);
		// valid for any block: stays in the pool
		let valid = bundle_of(txs.iter().skip(1).take(1), 0);
		let (expired_hash, valid_hash) = (expired.hash(), valid.hash());

		pool.insert(Order::Bundle(expired));
		pool.insert(Order::Bundle(valid));

		let tip = SealedHeader::seal_slow(alloy::consensus::Header {
			number: 2,
			..Default::default()
		});
		pool.remove_invalidated_orders(&tip);

		// the member of the expired bundle is dropped, the other one is not
		assert_eq!(dropped_flags(&pool, &[x, y]), vec![true, false]);
		assert!(pool.inner.orders.get(&expired_hash).is_none());
		assert!(pool.inner.orders.get(&valid_hash).is_some());
	}

	#[test]
	fn unknown_order_hashes_are_treated_as_transaction_hashes() {
		let pool = OrderPool::<Ethereum>::default();
		let tx = hash(15);

		pool
			.lifecycle()
			.record_received(tx, TxSource::Mempool, Instant::now());
		pool.report_inclusion_attempt(tx, payload(1));
		pool.report_inclusion_success(tx, payload(1));

		let lifecycle = pool.lifecycle().get(&tx).expect("tracked");
		assert!(lifecycle.first_considered().is_some());
		assert!(lifecycle.first_included().is_some());
	}

	/// A sealed block with the given number and timestamp that contains the
	/// given transactions.
	fn sealed_block<'a>(
		number: u64,
		timestamp: u64,
		txs: impl IntoIterator<Item = &'a Recovered<types::Transaction<Ethereum>>>,
	) -> SealedBlock<types::Block<Ethereum>> {
		SealedBlock::seal_parts(
			Header {
				number,
				timestamp,
				..Default::default()
			},
			BlockBody {
				transactions: txs
					.into_iter()
					.cloned()
					.map(Recovered::into_inner)
					.collect(),
				..Default::default()
			},
		)
	}

	#[test]
	fn committed_and_reverted_blocks_reach_the_bundle_members() {
		let pool = OrderPool::<Ethereum>::default();
		let (txs, [x, y, z]) = three_txs(6);
		let bundle = bundle_of(txs.iter(), 0);
		let bundle_hash = bundle.hash();
		pool.insert(Order::Bundle(bundle));

		// a block that contains two of the three members is committed
		let block = sealed_block(7, 1_700_000_007, txs.iter().take(2));
		pool.report_committed_block(&block);

		let [mined_x, mined_y, orphan_z] = lifecycles(&pool, &[x, y, z])
			.try_into()
			.expect("three lifecycles");
		// the members in the block are mined with the block's number and
		// timestamp, and not dropped although the bundle was evicted with them
		assert!([&mined_x, &mined_y].iter().all(|lifecycle| {
			lifecycle.mined().map(|mined| mined.block_number()) == Some(7)
				&& lifecycle.mined().map(|mined| mined.block_timestamp())
					== Some(1_700_000_007)
				&& lifecycle.dropped().is_none()
		}));
		// the third member is orphaned by the eviction of its bundle: dropped
		assert_eq!(orphan_z.mined(), None);
		assert!(orphan_z.dropped().is_some());
		// the bundle left the pool
		assert!(pool.inner.orders.get(&bundle_hash).is_none());
		assert!(pool.inner.tx_map.is_empty());

		// the block is reverted by a reorg: the two members are no longer
		// mined; they stay not dropped because a revert only clears the mined
		// stage (nothing evicts them again, they left the pool with the bundle)
		pool.report_reverted_block(&block);
		let [reverted_x, reverted_y, still_orphan_z] =
			lifecycles(&pool, &[x, y, z])
				.try_into()
				.expect("three lifecycles");
		assert!([&reverted_x, &reverted_y].iter().all(|lifecycle| {
			lifecycle.mined().is_none()
				&& lifecycle.dropped().is_none()
				&& lifecycle.reorg_count() == 1
		}));
		assert_eq!(still_orphan_z.reorg_count(), 0);
		assert!(still_orphan_z.dropped().is_some());

		// mined again in the new chain, this time all three
		let block = sealed_block(8, 1_700_000_019, txs.iter());
		pool.report_committed_block(&block);
		let remined = lifecycles(&pool, &[x, y, z]);
		assert!(remined.iter().all(|lifecycle| {
			lifecycle.mined().map(|mined| mined.block_number()) == Some(8)
				&& lifecycle.dropped().is_none()
		}));
		assert_eq!(pool.lifecycle().snapshot().len(), 3);
	}
}
