//! Ethereum receipt builder implementation

use crate::{platform::*, reth::ethereum::EthereumReceipt};

/// Ethereum receipt builder.
///
/// Builds standard Ethereum receipts that include:
/// - Transaction type (Legacy, EIP-2930, EIP-1559, EIP-7702)
/// - Success status (EIP-658)
/// - Cumulative gas used
/// - Transaction logs
pub struct EthereumReceiptBuilder;

impl ReceiptBuilder<Ethereum> for EthereumReceiptBuilder {
	fn build_receipt(ctx: ReceiptBuilderCtx<'_, Ethereum>) -> types::Receipt<Ethereum> {
		EthereumReceipt {
			tx_type: ctx.tx.tx_type(),
			// Success flag was added in `EIP-658: Embedding transaction status code in
			// receipts`.
			success: ctx.result.is_success(),
			cumulative_gas_used: ctx.cumulative_gas_used,
			logs: ctx.result.logs().to_vec(),
		}
	}
}