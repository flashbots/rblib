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
	reth::{
		ethereum::primitives::SignedTransaction,
		node::builder::{FullNodeComponents, rpc::RpcContext},
		rpc::api::eth::EthApiTypes,
	},
	serde::{Deserialize, Serialize},
	tracing::trace,
};

impl<P: PlatformWithRpcTypes> OrderPool<P> {
	/// Installs `OrderPool` RPC methods into a reth node.
	///
	/// This method will replace all ethereum api modules that are responsible for
	/// receiving new transactions and bundles and reporting on their status.
	///
	/// This includes:
	/// - `eth_sendRawTransaction`
	/// - `eth_sendBundle`
	///
	/// Attaching those RPC methods into a reth node will make all transactions
	/// and bundles received through those methods be inserted into the order pool
	/// instead of whatever mempool implementation the node is using.
	pub fn attach_rpc<Node, EthApi>(
		&self,
		rpc_ctx: &mut RpcContext<Node, EthApi>,
	) -> eyre::Result<()>
	where
		Node: FullNodeComponents<Types = types::NodeTypes<P>>,
		EthApi: EthApiTypes,
	{
		rpc_ctx
			.modules
			.add_or_replace_configured(BundlesRpcApi::new(self).into_rpc())?;

		rpc_ctx
			.modules
			.add_or_replace_configured(TransactionsRpcApi::new(self).into_rpc())?;

		Ok(())
	}
}

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
		let bundle_hash = bundle.hash();
		trace!(hash = %bundle_hash, "eth_sendBundle received: {bundle:?}");

		let order = Order::Bundle(bundle);

		if !self.pool.insert(order) {
			// todo: explore better (more specific) error codes
			return Err(ErrorObject::borrowed(
				ErrorCode::InvalidParams.code(),
				"Bundle is ineligible for inclusion",
				None,
			));
		}

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
			.inspect(|tx| {
				trace!(hash = %tx.tx_hash(), "eth_sendRawTransaction received: {tx:?}");
			})
			.inspect_err(|e| {
				debug!(error = %e, data = %bytes, "eth_sendRawTransaction failed to decode tx");
			})
			.map_err(|e| {
				ErrorObject::owned(
					ErrorCode::InvalidParams.code(),
					e.to_string(),
					Some(bytes.clone()),
				)
			})?;

		let tx: types::Transaction<P> = decoded.into();
		let tx = tx
			.try_into_recovered()
			.inspect_err(|tx| {
				debug!(tx = ?tx, "eth_sendRawTransaction: invalid signature");
			})
			.map_err(|_| {
				ErrorObject::owned(
					ErrorCode::InvalidParams.code(),
					"Invalid Signature".to_owned(),
					Some(bytes),
				)
			})?;

		let order = Order::Transaction(tx);
		let order_hash = order.hash();

		if !self.pool.insert(order.clone()) {
			// todo: explore better (more specific) error codes
			return Err(ErrorObject::borrowed(
				ErrorCode::InvalidParams.code(),
				"transaction is ineligible for inclusion",
				None,
			));
		}

		Ok(order_hash)
	}
}
