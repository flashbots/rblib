/// Example usage of `prioritized_pool` using pipelines
use {
	super::{
		super::{OrderpoolNonceSource, OrderpoolOrder},
		PrioritizedOrderpool,
		PrioritizedOrderpoolPriority,
	},
	crate::{
		alloy::{
			consensus::Transaction,
			primitives::{Address, B256},
		},
		orderpool2::{AccountNonce, BundleNonce},
		payload::CheckpointExt,
		prelude::{Bundle, Checkpoint, ControlFlow, Platform, Step, StepContext},
		reth,
	},
	parking_lot::Mutex,
	reth_ethereum::primitives::transaction::TxHashRef,
	reth_evm::revm::DatabaseRef,
	std::{
		marker::{PhantomData, Send, Sync},
		sync::Arc,
	},
};

#[derive(Clone)]
pub struct BundleWithNonces<B, P> {
	bundle: B,
	nonces: Vec<BundleNonce>,
	phantom: std::marker::PhantomData<P>,
}

impl<B, P> BundleWithNonces<B, P>
where
	B: Bundle<P>,
	P: Platform,
{
	fn new(bundle: B) -> Self {
		let txs = bundle.transactions();
		let mut nonces = Vec::with_capacity(txs.len());
		for tx in txs {
			nonces.push(BundleNonce {
				address: tx.signer(),
				nonce: tx.nonce(),
				optional: bundle.is_optional(tx.tx_hash()),
			});
		}
		// for each address we keep lowest nonce
		nonces
			.sort_by(|a, b| a.address.cmp(&b.address).then(a.nonce.cmp(&b.nonce)));
		nonces.dedup_by_key(|n| n.address);
		Self {
			bundle,
			nonces,
			phantom: PhantomData,
		}
	}
}

impl<B, P> OrderpoolOrder for BundleWithNonces<B, P>
where
	B: Bundle<P>,
	P: Platform,
{
	type ID = B256;

	fn id(&self) -> Self::ID {
		self.bundle.hash()
	}

	fn nonces(&self) -> Vec<BundleNonce> {
		self.nonces.clone()
	}
}

impl<P: Platform> OrderpoolNonceSource for Checkpoint<P> {
	type NonceError = reth::errors::ProviderError;

	fn nonce(&self, address: &Address) -> Result<u64, Self::NonceError> {
		Ok(
			self
				.basic_ref(*address)?
				.map(|acc| acc.nonce)
				.unwrap_or_default(),
		)
	}
}

pub struct EffectiveGasPriceOrdering<B, P> {
	effective_gas_price: u128,
	marker: PhantomData<(B, P)>,
}

impl<B, P> Clone for EffectiveGasPriceOrdering<B, P> {
	fn clone(&self) -> Self {
		Self {
			effective_gas_price: self.effective_gas_price,
			marker: PhantomData,
		}
	}
}

impl<B, P> PartialOrd for EffectiveGasPriceOrdering<B, P> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

impl<B, P> Ord for EffectiveGasPriceOrdering<B, P> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.effective_gas_price.cmp(&other.effective_gas_price)
	}
}

impl<B, P> PartialEq for EffectiveGasPriceOrdering<B, P> {
	fn eq(&self, other: &Self) -> bool {
		self.effective_gas_price == other.effective_gas_price
	}
}
impl<B, P> Eq for EffectiveGasPriceOrdering<B, P> {}

impl<B: Send + Sync + Bundle<P>, P: Send + Sync + Platform>
	PrioritizedOrderpoolPriority for EffectiveGasPriceOrdering<B, P>
{
	type Order = BundleWithNonces<B, P>;

	fn new(order: &Self::Order) -> Self {
		let mut total_gas_limit = 0u128;
		let mut accumulated_weighted_fee = 0u128;
		for tx in order.bundle.transactions() {
			if order.bundle.is_optional(tx.tx_hash()) {
				continue;
			}
			let max_fee = tx.max_fee_per_gas();
			let gas_limit = u128::from(tx.gas_limit());
			accumulated_weighted_fee += max_fee * gas_limit;
			total_gas_limit += gas_limit;
		}
		let effective_gas_price = if total_gas_limit != 0 {
			accumulated_weighted_fee / total_gas_limit
		} else {
			0
		};
		Self {
			effective_gas_price,
			marker: PhantomData,
		}
	}
}

type PrioritizedOrderpoolForPlatform<Priority, P: Platform> =
	PrioritizedOrderpool<Priority, BundleWithNonces<P::Bundle, P>>;

pub struct AppendOrdersByPriority<P, Pending, Priority>
where
	P: Platform,
	Pending: PendingOrdersSource<P>,
	Priority:
		PrioritizedOrderpoolPriority<Order = BundleWithNonces<P::Bundle, P>>,
{
	pending: Pending,
	prioritized_orderpool:
		Arc<Mutex<PrioritizedOrderpoolForPlatform<Priority, P>>>,
	marker: PhantomData<P>,
}

/// `PendingOrdersSource` is a source from which we can pull new bundles
/// One example of this would be a `best_transactions` iterator from mempool
pub trait PendingOrdersSource<P: Platform>: Send + Sync + 'static {
	/// If None is returned, we don't have new pending bundles yet
	fn try_get_next_pending(&self) -> Option<P::Bundle>;
}

impl<P, Pending, Priority> Step<P>
	for AppendOrdersByPriority<P, Pending, Priority>
where
	P: Platform,
	Pending: PendingOrdersSource<P>,
	Priority: PrioritizedOrderpoolPriority<Order = BundleWithNonces<P::Bundle, P>>
		+ 'static,
{
	fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		_ctx: StepContext<P>,
	) -> impl Future<Output = ControlFlow<P>> + Send {
		let mut orderpool = self.prioritized_orderpool.lock();
		// pull all new pending orders
		while let Some(order) = self.pending.try_get_next_pending() {
			orderpool
				.insert_order(BundleWithNonces::new(order), &payload)
				.unwrap_or_default();
		}
		// we clone because orderpool is modified during the execution
		let mut orderpool = orderpool.clone();
		let mut payload = payload;

		// try to include all orders
		while let Some(order) = orderpool.pop_order() {
			if let Ok(new) = payload.apply(order.bundle) {
				let changed_nonces: Vec<_> = new
					.changed_nonces()
					.into_iter()
					.map(|(account, nonce)| AccountNonce { account, nonce })
					.collect();

				orderpool
					.update_onchain_nonces(&changed_nonces, &new)
					.unwrap_or_default();
				payload = new;
			}
		}
		async { ControlFlow::Ok(payload) }
	}
}
