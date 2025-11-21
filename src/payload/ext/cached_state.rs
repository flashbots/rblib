//! Database adapters for payload building.
// Originated from reth https://github.com/paradigmxyz/reth/blob/b72bb6790a5f7ada75282e52b80f986d9717e698/crates/revm/src/cached.rs

use {
	crate::{
		alloy::primitives::{Address, B256, StorageKey, StorageValue},
		reth::{
			errors::ProviderResult,
			ethereum::provider::{
				AccountReader,
				BlockHashReader,
				BytecodeReader,
				HashedPostStateProvider,
				StateProofProvider,
				StateProvider,
				StateRootProvider,
				StorageRootProvider,
			},
		},
	},
	dashmap::DashMap,
	reth_ethereum::{
		evm::revm::db::BundleState,
		primitives::{Account, Bytecode},
		trie::{
			AccountProof,
			HashedPostState,
			HashedStorage,
			MultiProof,
			MultiProofTargets,
			StorageMultiProof,
			StorageProof,
			TrieInput,
			updates::TrieUpdates,
		},
	},
	std::sync::Arc,
};

/// A wrapper of a state provider and a shared cache.
pub struct CachedStateProvider<S> {
	/// The state provider
	state_provider: S,

	/// The caches used for the provider
	caches: ExecutionCache,
}

impl<S> CachedStateProvider<S>
where
	S: StateProvider,
{
	/// Creates a new [`CachedStateProvider`] from an [`ExecutionCache`], state
	/// provider, and [`CachedStateMetrics`].
	pub const fn new_with_caches(
		state_provider: S,
		caches: ExecutionCache,
	) -> Self {
		Self {
			state_provider,
			caches,
		}
	}
}

impl<S: AccountReader> AccountReader for CachedStateProvider<S> {
	fn basic_account(
		&self,
		address: &Address,
	) -> ProviderResult<Option<Account>> {
		if let Some(res) = self.caches.account.get(address) {
			return Ok(*res);
		}

		let res = self.state_provider.basic_account(address)?;
		self.caches.account.insert(*address, res);
		Ok(res)
	}
}

/// Represents the status of a storage slot in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotStatus {
	/// The account's storage cache doesn't exist.
	NotCached,
	/// The storage slot exists in cache and is empty (value is zero).
	Empty,
	/// The storage slot exists in cache and has a specific non-zero value.
	Value(StorageValue),
}

impl<S: StateProvider> StateProvider for CachedStateProvider<S> {
	fn storage(
		&self,
		account: Address,
		storage_key: StorageKey,
	) -> ProviderResult<Option<StorageValue>> {
		match self.caches.get_storage(&account, &storage_key) {
			SlotStatus::NotCached => {
				let final_res = self.state_provider.storage(account, storage_key)?;
				self.caches.insert_storage(account, storage_key, final_res);
				Ok(final_res)
			}
			SlotStatus::Empty => Ok(None),
			SlotStatus::Value(value) => Ok(Some(value)),
		}
	}
}

impl<S: BytecodeReader> BytecodeReader for CachedStateProvider<S> {
	fn bytecode_by_hash(
		&self,
		code_hash: &B256,
	) -> ProviderResult<Option<Bytecode>> {
		if let Some(res) = self.caches.code.get(code_hash).as_deref() {
			return Ok(res.clone());
		}

		let final_res = self.state_provider.bytecode_by_hash(code_hash)?;
		self.caches.code.insert(*code_hash, final_res.clone());
		Ok(final_res)
	}
}

impl<S: StateRootProvider> StateRootProvider for CachedStateProvider<S> {
	fn state_root(&self, hashed_state: HashedPostState) -> ProviderResult<B256> {
		self.state_provider.state_root(hashed_state)
	}

	fn state_root_from_nodes(&self, input: TrieInput) -> ProviderResult<B256> {
		self.state_provider.state_root_from_nodes(input)
	}

	fn state_root_with_updates(
		&self,
		hashed_state: HashedPostState,
	) -> ProviderResult<(B256, TrieUpdates)> {
		self.state_provider.state_root_with_updates(hashed_state)
	}

	fn state_root_from_nodes_with_updates(
		&self,
		input: TrieInput,
	) -> ProviderResult<(B256, TrieUpdates)> {
		self
			.state_provider
			.state_root_from_nodes_with_updates(input)
	}
}

impl<S: StateProofProvider> StateProofProvider for CachedStateProvider<S> {
	fn proof(
		&self,
		input: TrieInput,
		address: Address,
		slots: &[B256],
	) -> ProviderResult<AccountProof> {
		self.state_provider.proof(input, address, slots)
	}

	fn multiproof(
		&self,
		input: TrieInput,
		targets: MultiProofTargets,
	) -> ProviderResult<MultiProof> {
		self.state_provider.multiproof(input, targets)
	}

	fn witness(
		&self,
		input: TrieInput,
		target: HashedPostState,
	) -> ProviderResult<Vec<crate::alloy::primitives::Bytes>> {
		self.state_provider.witness(input, target)
	}
}

impl<S: StorageRootProvider> StorageRootProvider for CachedStateProvider<S> {
	fn storage_root(
		&self,
		address: Address,
		hashed_storage: HashedStorage,
	) -> ProviderResult<B256> {
		self.state_provider.storage_root(address, hashed_storage)
	}

	fn storage_proof(
		&self,
		address: Address,
		slot: B256,
		hashed_storage: HashedStorage,
	) -> ProviderResult<StorageProof> {
		self
			.state_provider
			.storage_proof(address, slot, hashed_storage)
	}

	/// Generate a storage multiproof for multiple storage slots.
	///
	/// A **storage multiproof** is a cryptographic proof that can verify the
	/// values of multiple storage slots for a single account in a single
	/// verification step. Instead of generating separate proofs for each slot
	/// (which would be inefficient), a multiproof bundles the necessary trie
	/// nodes to prove all requested slots.
	///
	/// ## How it works:
	/// 1. Takes an account address and a list of storage slot keys
	/// 2. Traverses the account's storage trie to collect proof nodes
	/// 3. Returns a [`StorageMultiProof`] containing the minimal set of trie
	///    nodes needed to verify all the requested storage slots
	fn storage_multiproof(
		&self,
		address: Address,
		slots: &[B256],
		hashed_storage: HashedStorage,
	) -> ProviderResult<StorageMultiProof> {
		self
			.state_provider
			.storage_multiproof(address, slots, hashed_storage)
	}
}

impl<S: BlockHashReader> BlockHashReader for CachedStateProvider<S> {
	fn block_hash(
		&self,
		number: crate::alloy::primitives::BlockNumber,
	) -> ProviderResult<Option<B256>> {
		self.state_provider.block_hash(number)
	}

	fn canonical_hashes_range(
		&self,
		start: crate::alloy::primitives::BlockNumber,
		end: crate::alloy::primitives::BlockNumber,
	) -> ProviderResult<Vec<B256>> {
		self.state_provider.canonical_hashes_range(start, end)
	}
}

impl<S: HashedPostStateProvider> HashedPostStateProvider
	for CachedStateProvider<S>
{
	fn hashed_post_state(&self, bundle_state: &BundleState) -> HashedPostState {
		self.state_provider.hashed_post_state(bundle_state)
	}
}

/// Execution cache used during block processing.
///
/// Optimizes state access by maintaining in-memory copies of frequently
/// accessed accounts, storage slots, and bytecode. Works in conjunction with
/// prewarming to reduce database I/O during block execution.
#[derive(Debug, Clone, Default)]
pub struct ExecutionCache {
	/// Cache for contract bytecode, keyed by code hash.
	code: DashMap<B256, Option<Bytecode>>,

	/// Per-account storage cache: outer cache keyed by Address, inner cache
	/// tracks that account’s storage slots.
	storage: DashMap<Address, Arc<AccountStorageCache>>,

	/// Cache for basic account information (nonce, balance, code hash).
	account: DashMap<Address, Option<Account>>,
}

impl ExecutionCache {
	/// Get storage value from hierarchical cache.
	///
	/// Returns a `SlotStatus` indicating whether:
	/// - `NotCached`: The account's storage cache doesn't exist
	/// - `Empty`: The slot exists in the account's cache but is empty
	/// - `Value`: The slot exists and has a specific value
	pub fn get_storage(&self, address: &Address, key: &StorageKey) -> SlotStatus {
		match self.storage.get(address) {
			None => SlotStatus::NotCached,
			Some(account_cache) => account_cache.get_storage(key),
		}
	}

	/// Insert storage value into hierarchical cache
	pub fn insert_storage(
		&self,
		address: Address,
		key: StorageKey,
		value: Option<StorageValue>,
	) {
		self.insert_storage_bulk(address, [(key, value)]);
	}

	/// Insert multiple storage values into hierarchical cache for a single
	/// account
	///
	/// This method is optimized for inserting multiple storage values for the
	/// same address by doing the account cache lookup only once instead of for
	/// each key-value pair.
	pub fn insert_storage_bulk<I>(&self, address: Address, storage_entries: I)
	where
		I: IntoIterator<Item = (StorageKey, Option<StorageValue>)>,
	{
		let account_cache =
			self.storage.entry(address).or_insert_with(Default::default);

		for (key, value) in storage_entries {
			account_cache.slots.insert(key, value);
		}
	}

	/// Returns the total number of storage slots cached across all accounts
	pub fn total_storage_slots(&self) -> usize {
		self.storage.iter().map(|addr| addr.len()).sum()
	}

	/// Inserts the post-execution state changes into the cache.
	///
	/// This method is called after transaction execution to update the cache with
	/// the touched and modified state. The insertion order is critical:
	///
	/// 1. Bytecodes: Insert contract code first
	/// 2. Storage slots: Update storage values for each account
	/// 3. Accounts: Update account info (nonce, balance, code hash)
	///
	/// ## Why This Order Matters
	///
	/// Account information references bytecode via code hash. If we update
	/// accounts before bytecode, we might create cache entries pointing to
	/// non-existent code. The current order ensures cache consistency.
	///
	/// ## Error Handling
	///
	/// Returns an error if the state updates are inconsistent and should be
	/// discarded.
	#[expect(clippy::result_unit_err)]
	pub fn insert_state(&self, state_updates: &BundleState) -> Result<(), ()> {
		// Insert bytecodes
		for (code_hash, bytecode) in &state_updates.contracts {
			self
				.code
				.insert(*code_hash, Some(Bytecode(bytecode.clone())));
		}

		for (addr, account) in &state_updates.state {
			// If the account was not modified, as in not changed and not destroyed,
			// then we have nothing to do w.r.t. this particular account and can
			// move on
			if account.status.is_not_modified() {
				continue;
			}

			// If the account was destroyed, invalidate from the account / storage
			// caches
			if account.was_destroyed() {
				// Invalidate the account cache entry if destroyed
				self.account.remove(addr);
				self.storage.remove(addr);
				continue;
			}

			// If we have an account that was modified, but it has a `None` account
			// info, some wild error has occurred because this state should be
			// unrepresentable. An account with `None` current info, should be
			// destroyed.
			let Some(ref account_info) = account.info else {
				tracing::trace!(
					?account,
					"Account with None account info found in state updates"
				);
				return Err(());
			};

			// Now we iterate over all storage and make updates to the cached storage
			// values Use bulk insertion to optimize cache lookups - only lookup
			// the account cache once instead of for each storage key
			let storage_entries =
				account.storage.iter().map(|(storage_key, slot)| {
					// We convert the storage key from U256 to B256 because that is how
					// it's represented in the cache
					((*storage_key).into(), Some(slot.present_value))
				});
			self.insert_storage_bulk(*addr, storage_entries);

			// Insert will update if present, so we just use the new account info as
			// the new value for the account cache
			self
				.account
				.insert(*addr, Some(Account::from(account_info)));
		}

		Ok(())
	}
}

/// Cache for an individual account's storage slots.
///
/// This represents the second level of the hierarchical storage cache.
/// Each account gets its own `AccountStorageCache` to store accessed storage
/// slots.
#[derive(Debug, Clone)]
pub(super) struct AccountStorageCache {
	/// Map of storage keys to their cached values.
	slots: DashMap<StorageKey, Option<StorageValue>>,
}

impl AccountStorageCache {
	/// Create a new [`AccountStorageCache`]
	pub(crate) fn new() -> Self {
		Self {
			slots: DashMap::new(),
		}
	}

	/// Get a storage value from this account's cache.
	/// - `NotCached`: The slot is not in the cache
	/// - `Empty`: The slot is empty
	/// - `Value`: The slot has a specific value
	pub(crate) fn get_storage(&self, key: &StorageKey) -> SlotStatus {
		match self.slots.get(key).as_deref() {
			None => SlotStatus::NotCached,
			Some(None) => SlotStatus::Empty,
			Some(Some(value)) => SlotStatus::Value(*value),
		}
	}

	/// Insert a storage value
	#[expect(dead_code)]
	pub(crate) fn insert_storage(
		&self,
		key: StorageKey,
		value: Option<StorageValue>,
	) {
		self.slots.insert(key, value);
	}

	/// Returns the number of slots in the cache
	pub(crate) fn len(&self) -> usize {
		self.slots.len()
	}
}

impl Default for AccountStorageCache {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::{
			alloy::primitives::U256,
			reth::providers::test_utils::{ExtendedAccount, MockEthProvider},
		},
	};

	#[test]
	fn test_empty_storage_cached_state_provider() {
		// make sure when we have an empty value in storage, we return `Empty` and
		// not `NotCached`
		let address = Address::random();
		let storage_key = StorageKey::random();
		let account = ExtendedAccount::new(0, U256::ZERO);

		// note there is no storage here
		let provider = MockEthProvider::default();
		provider.extend_accounts(vec![(address, account)]);

		let caches = ExecutionCache::default();
		let state_provider = CachedStateProvider::new_with_caches(provider, caches);

		// check that the storage is empty
		let res = state_provider.storage(address, storage_key);
		assert!(res.is_ok());
		assert_eq!(res.unwrap(), None);
	}

	#[test]
	fn test_uncached_storage_cached_state_provider() {
		// make sure when we have something uncached, we get the cached value
		let address = Address::random();
		let storage_key = StorageKey::random();
		let storage_value = U256::from(1);
		let account = ExtendedAccount::new(0, U256::ZERO)
			.extend_storage(vec![(storage_key, storage_value)]);

		// note that we extend storage here with one value
		let provider = MockEthProvider::default();
		provider.extend_accounts(vec![(address, account)]);

		let caches = ExecutionCache::default();
		let state_provider = CachedStateProvider::new_with_caches(provider, caches);

		// check that the storage returns the expected value
		let res = state_provider.storage(address, storage_key);
		assert!(res.is_ok());
		assert_eq!(res.unwrap(), Some(storage_value));
	}

	#[test]
	fn test_get_storage_populated() {
		// make sure when we have something cached, we get the cached value in the
		// `SlotStatus`
		let address = Address::random();
		let storage_key = StorageKey::random();
		let storage_value = U256::from(1);

		// insert into caches directly
		let caches = ExecutionCache::default();
		caches.insert_storage(address, storage_key, Some(storage_value));

		// check that the storage returns the cached value
		let slot_status = caches.get_storage(&address, &storage_key);
		assert_eq!(slot_status, SlotStatus::Value(storage_value));
	}

	#[test]
	fn test_get_storage_not_cached() {
		// make sure when we have nothing cached, we get the `NotCached` value in
		// the `SlotStatus`
		let storage_key = StorageKey::random();
		let address = Address::random();

		// just create empty caches
		let caches = ExecutionCache::default();

		// check that the storage is not cached
		let slot_status = caches.get_storage(&address, &storage_key);
		assert_eq!(slot_status, SlotStatus::NotCached);
	}

	#[test]
	fn test_get_storage_empty() {
		// make sure when we insert an empty value to the cache, we get the `Empty`
		// value in the `SlotStatus`
		let address = Address::random();
		let storage_key = StorageKey::random();

		// insert into caches directly
		let caches = ExecutionCache::default();
		caches.insert_storage(address, storage_key, None);

		// check that the storage is empty
		let slot_status = caches.get_storage(&address, &storage_key);
		assert_eq!(slot_status, SlotStatus::Empty);
	}
}
