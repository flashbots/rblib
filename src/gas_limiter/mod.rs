use std::{cmp::min, marker::PhantomData, time::Instant};

use alloy_primitives::Address;
use dashmap::DashMap;

use crate::{
	alloy::consensus::Transaction,
	gas_limiter::metrics::GasLimiterMetrics,
	pool::Order,
	prelude::*,
};

pub mod args;
pub mod error;
mod metrics;


pub use args::GasLimiterArgs;
pub use error::GasLimitError;

#[derive(Debug)]
pub struct AddressGasLimiter {
    inner: Option<AddressGasLimiterInner>,
}

#[derive(Debug)]
struct AddressGasLimiterInner {
    config: GasLimiterArgs,
    address_buckets: DashMap<Address, TokenBucket>,
    metrics: GasLimiterMetrics,
}

#[derive(Debug, Clone)]
struct TokenBucket {
    capacity: u64,
    available: u64,
}

/// A filter wrapper around AddressGasLimiter that can be used with AppendOrders.
/// This provides a convenient way to integrate per-address gas limiting into
/// the payload building pipeline.
#[derive(Debug)]
pub struct GasLimitFilter<P: Platform> {
	limiter: AddressGasLimiter,
	_phantom: PhantomData<P>,
}

impl<P: Platform> GasLimitFilter<P> {
	/// Creates a new gas limit filter with the given configuration.
	pub fn new(config: GasLimiterArgs) -> Self {
		Self {
			limiter: AddressGasLimiter::new(config),
			_phantom: PhantomData,
		}
	}

	/// Creates a filter closure that can be used with AppendOrders::with_filter().
	/// The filter will reject orders if any of their transactions would exceed
	/// the per-address gas limit. The filter automatically refreshes gas buckets
	/// when it detects a new block number.
	pub fn create_filter(
		self,
	) -> impl Fn(&Checkpoint<P>, &Order<P>) -> bool + Send + Sync + 'static {
		use std::sync::atomic::{AtomicU64, Ordering};
		
		// Track the last block number we refreshed for
		let last_refreshed_block = AtomicU64::new(0);
		
		move |payload: &Checkpoint<P>, order: &Order<P>| -> bool {
			// Get current block number from the payload's context
			let current_block = payload.block().number();
			let last_block = last_refreshed_block.load(Ordering::Relaxed);
			
			// Refresh gas buckets if we're on a new block
			if current_block > last_block {
				self.limiter.refresh(current_block);
				last_refreshed_block.store(current_block, Ordering::Relaxed);
			}
			
			// For each transaction in the order, check if the signer has enough gas budget
			for tx in order.transactions() {
				let signer = tx.signer();
				// Access the underlying transaction to get gas limit
                
				let gas_limit = tx.gas_limit();

				// Try to consume gas from the signer's bucket
				if self.limiter.consume_gas(signer, gas_limit).is_err() {
					// Not enough gas in bucket, reject this order
					return true; // true means "skip this order"
				}
			}
			false // false means "don't skip this order"
		}
	}

	/// Refreshes the gas buckets, typically called at the start of each block.
	/// This refills buckets and performs garbage collection of stale entries.
	pub fn refresh(&self, block_number: u64) {
		self.limiter.refresh(block_number);
	}
}

impl AddressGasLimiter {
    pub fn new(config: GasLimiterArgs) -> Self {
        Self {
            inner: AddressGasLimiterInner::try_new(config),
        }
    }

    /// Check if there's enough gas for this address and consume it. Returns
    /// Ok(()) if there's enough otherwise returns an error.
    pub fn consume_gas(&self, address: Address, gas_requested: u64) -> Result<(), GasLimitError> {
        if let Some(inner) = &self.inner {
            inner.consume_gas(address, gas_requested)
        } else {
            Ok(())
        }
    }

    /// Should be called upon each new block. Refills buckets/Garbage collection
    pub fn refresh(&self, block_number: u64) {
        if let Some(inner) = self.inner.as_ref() {
            inner.refresh(block_number)
        }
    }
}

impl AddressGasLimiterInner {
    fn try_new(config: GasLimiterArgs) -> Option<Self> {
        if !config.gas_limiter_enabled {
            return None;
        }

        Some(Self {
            config,
            address_buckets: Default::default(),
            metrics: Default::default(),
        })
    }

    fn consume_gas_inner(
        &self,
        address: Address,
        gas_requested: u64,
    ) -> Result<bool, GasLimitError> {
        let mut created_new_bucket = false;
        let mut bucket = self
            .address_buckets
            .entry(address)
            // if we don't find a bucket we need to initialize a new one
            .or_insert_with(|| {
                created_new_bucket = true;
                TokenBucket::new(self.config.max_gas_per_address)
            });

        if gas_requested > bucket.available {
            return Err(GasLimitError::AddressLimitExceeded {
                address,
                requested: gas_requested,
                available: bucket.available,
            });
        }

        bucket.available -= gas_requested;

        Ok(created_new_bucket)
    }

    fn consume_gas(&self, address: Address, gas_requested: u64) -> Result<(), GasLimitError> {
        let start = Instant::now();
        let result = self.consume_gas_inner(address, gas_requested);

        self.metrics.record_gas_check(&result, start.elapsed());

        result.map(|_| ())
    }

    fn refresh_inner(&self, block_number: u64) -> usize {
        let active_addresses = self.address_buckets.len();

        self.address_buckets.iter_mut().for_each(|mut bucket| {
            bucket.available = min(
                bucket.capacity,
                bucket.available + self.config.refill_rate_per_block,
            )
        });

        // Only clean up stale buckets every `cleanup_interval` blocks
        if block_number % self.config.cleanup_interval_blocks == 0 {
            self.address_buckets
                .retain(|_, bucket| bucket.available <= bucket.capacity);
        }

        active_addresses - self.address_buckets.len()
    }

    fn refresh(&self, block_number: u64) {
        let start = Instant::now();
        let removed_addresses = self.refresh_inner(block_number);

        self.metrics
            .record_refresh(removed_addresses, start.elapsed());
    }
}

impl TokenBucket {
    fn new(capacity: u64) -> Self {
        Self {
            capacity,
            available: capacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;

    fn create_test_config(max_gas: u64, refill_rate: u64, cleanup_interval: u64) -> GasLimiterArgs {
        GasLimiterArgs {
            gas_limiter_enabled: true,
            max_gas_per_address: max_gas,
            refill_rate_per_block: refill_rate,
            cleanup_interval_blocks: cleanup_interval,
        }
    }

    fn test_address() -> Address {
        Address::from([1u8; 20])
    }

    #[test]
    fn test_basic_refill() {
        let config = create_test_config(1000, 200, 10);
        let limiter = AddressGasLimiter::new(config);

        // Consume all gas
        assert!(limiter.consume_gas(test_address(), 1000).is_ok());
        assert!(limiter.consume_gas(test_address(), 1).is_err());

        // Refill and check available gas increased
        limiter.refresh(1);
        assert!(limiter.consume_gas(test_address(), 200).is_ok());
        assert!(limiter.consume_gas(test_address(), 1).is_err());
    }

    #[test]
    fn test_over_capacity_request() {
        let config = create_test_config(1000, 100, 10);
        let limiter = AddressGasLimiter::new(config);

        // Request more than capacity should fail
        let result = limiter.consume_gas(test_address(), 1500);
        assert!(result.is_err());

        if let Err(GasLimitError::AddressLimitExceeded { available, .. }) = result {
            assert_eq!(available, 1000);
        }

        // Bucket should still be full after failed request
        assert!(limiter.consume_gas(test_address(), 1000).is_ok());
    }

    #[test]
    fn test_multiple_users() {
        // Simulate more realistic scenario
        let config = create_test_config(10_000_000, 1_000_000, 100); // 10M max, 1M refill
        let limiter = AddressGasLimiter::new(config);

        let searcher1 = Address::from([0x1; 20]);
        let searcher2 = Address::from([0x2; 20]);
        let attacker = Address::from([0x3; 20]);

        // Normal searchers use reasonable amounts
        assert!(limiter.consume_gas(searcher1, 500_000).is_ok());
        assert!(limiter.consume_gas(searcher2, 750_000).is_ok());

        // Attacker tries to consume massive amounts
        assert!(limiter.consume_gas(attacker, 15_000_000).is_err()); // Should fail - over capacity
        assert!(limiter.consume_gas(attacker, 5_000_000).is_ok()); // Should succeed - within capacity

        // Attacker tries to consume more
        assert!(limiter.consume_gas(attacker, 6_000_000).is_err()); // Should fail - would exceed remaining

        // New block - refill
        limiter.refresh(1);

        // Everyone should get some gas back
        assert!(limiter.consume_gas(searcher1, 1_000_000).is_ok()); // Had 9.5M + 1M refill, now 9.5M
        assert!(limiter.consume_gas(searcher2, 1_000_000).is_ok()); // Had 9.25M + 1M refill, now 9.25M  
        assert!(limiter.consume_gas(attacker, 1_000_000).is_ok()); // Had 5M + 1M refill, now 5M
    }

    #[test]
    fn test_gas_limit_filter_creation() {
        // Test that we can create a gas limit filter
        let config = create_test_config(100_000, 25_000, 10);
        let filter = GasLimitFilter::<crate::platform::Ethereum>::new(config);
        
        // Test that we can create the filter closure
        let _filter_fn = filter.create_filter();
        
        // This test mainly ensures the types compile correctly
        assert!(true);
    }

    #[test] 
    fn test_disabled_gas_limiter() {
        // Test that disabled gas limiter allows all transactions
        let config = GasLimiterArgs {
            gas_limiter_enabled: false,
            max_gas_per_address: 1, // Very low limit, should be ignored
            refill_rate_per_block: 1,
            cleanup_interval_blocks: 10,
        };
        
        let limiter = AddressGasLimiter::new(config);
        
        // Should allow any amount of gas when disabled
        assert!(limiter.consume_gas(test_address(), 1_000_000).is_ok());
        assert!(limiter.consume_gas(test_address(), 1_000_000).is_ok());
    }
}
