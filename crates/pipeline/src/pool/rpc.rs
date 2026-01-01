use {
	super::{Order, OrderPool},
	crate::{alloy, prelude::*},
	alloy::primitives::B256,
	jsonrpsee::{
		core::{RpcResult, async_trait},
		proc_macros::rpc,
		tracing::debug,
		types::{ErrorCode, ErrorObject},
	},
	serde::{Deserialize, Serialize},
};

pub(super) struct BundleRpcApi<P: Platform> {
	pool: OrderPool<P>,
}

impl<P: Platform> BundleRpcApi<P> {
	pub(super) fn new(pool: &OrderPool<P>) -> Self {
		Self { pool: pool.clone() }
	}
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BundleResult {
	#[serde(rename = "bundleHash")]
	pub bundle_hash: B256,
}

#[rpc(server, client, namespace = "eth")]
pub trait BundlesApi<P: Platform> {
	#[method(name = "sendBundle")]
	async fn send_bundle(
		&self,
		bundle: types::Bundle<P>,
	) -> RpcResult<BundleResult>;
}

#[async_trait]
impl<P: Platform> BundlesApiServer<P> for BundleRpcApi<P> {
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
