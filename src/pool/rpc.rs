use {
	super::{Order, OrderPool},
	crate::{alloy, prelude::*, reth},
	alloy::{
		eips::Decodable2718,
		primitives::{B256, Bytes},
	},
	jsonrpsee::{
		core::{RpcResult, async_trait},
		proc_macros::rpc,
		tracing::debug,
		types::{ErrorCode, ErrorObject},
	},
	reth::ethereum::primitives::SignedTransaction,
	serde::{Deserialize, Serialize},
};

pub(super) struct BundlesRpcApi<P: PlatformWithRpcTypes> {
	pool: OrderPool<P>,
}

impl<P: PlatformWithRpcTypes> BundlesRpcApi<P> {
	pub fn new(pool: &OrderPool<P>) -> Self {
		Self { pool: pool.clone() }
	}
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BundleResult {
	#[serde(rename = "bundleHash")]
	pub bundle_hash: B256,
}

#[rpc(server, client, namespace = "eth")]
pub trait BundlesApi<P: PlatformWithRpcTypes> {
	#[method(name = "sendBundle")]
	async fn send_bundle(
		&self,
		bundle: types::Bundle<P>,
	) -> RpcResult<BundleResult>;
}

#[async_trait]
impl<P: PlatformWithRpcTypes> BundlesApiServer<P> for BundlesRpcApi<P> {
	async fn send_bundle(
		&self,
		bundle: types::Bundle<P>,
	) -> RpcResult<BundleResult> {
		let invalid_param_err = || {
			ErrorObject::borrowed(
				ErrorCode::InvalidParams.code(),
				"bundle is ineligible for inclusion",
				None,
			)
		};

		// empty bundles are never valid
		if bundle.transactions().is_empty() {
			return Err(invalid_param_err());
		}

		// If we can tell that the bundle is permanently ineligible for inclusion in
		// any future block, we reject it immediately without adding it to the
		// pool.
		if self.pool.is_permanently_ineligible(&bundle) {
			return Err(invalid_param_err());
		}

		let bundle_hash = bundle.hash();
		debug!(hash = %bundle_hash, "eth_sendBundle received: {bundle:?}");
		self.pool.insert(Order::Bundle(bundle));
		Ok(BundleResult { bundle_hash })
	}
}

pub(super) struct TransactionsRpcApi<P: PlatformWithRpcTypes> {
	pool: OrderPool<P>,
}

impl<P: PlatformWithRpcTypes> TransactionsRpcApi<P> {
	pub fn new(pool: &OrderPool<P>) -> Self {
		Self { pool: pool.clone() }
	}
}

#[rpc(server, client, namespace = "eth")]
pub trait TransactionsApi<P: PlatformWithRpcTypes> {
	#[method(name = "sendRawTransaction")]
	async fn send_raw_transaction(&self, bytes: Bytes) -> RpcResult<B256>;
}

#[async_trait]
impl<P: PlatformWithRpcTypes> TransactionsApiServer<P>
	for TransactionsRpcApi<P>
{
	async fn send_raw_transaction(&self, bytes: Bytes) -> RpcResult<B256> {
		let decoded = types::TxEnvelope::<P>::decode_2718(&mut &bytes[..])
			.map_err(|e| {
				ErrorObject::owned(
					ErrorCode::InvalidParams.code(),
					e.to_string(),
					Some(bytes.clone()),
				)
			})?;

		let tx: types::Transaction<P> = decoded.into();
		let rtx = tx.try_into_recovered().map_err(|_| {
			ErrorObject::owned(
				ErrorCode::InvalidParams.code(),
				"Invalid Signature".to_owned(),
				Some(bytes),
			)
		})?;

		let txhash = *rtx.tx_hash();
		self.pool.insert(Order::Transaction(rtx));

		Ok(txhash)
	}
}
