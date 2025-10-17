//! Optimism receipt builder implementation

use crate::{alloy::consensus::*, platform::*, reth::optimism::primitives::*};

/// Optimism receipt builder.
///
/// Builds Optimism-specific receipts that include:
/// - Standard receipt types (Legacy, EIP-2930, EIP-1559, EIP-7702)
/// - Deposit receipts for L1 → L2 transactions
///
/// Note: Deposit receipts are not supported in the current execution context
/// because deposit transactions are handled by the sequencer and injected
/// via payload attributes, not executed through the standard transaction
/// execution path.
pub struct OptimismReceiptBuilder;

impl ReceiptBuilder<Optimism> for OptimismReceiptBuilder {
	fn build_receipt(ctx: ReceiptBuilderCtx<'_, Optimism>) -> types::Receipt<Optimism> {
		let tx_type = ctx.tx.tx_type();

		// Build the base receipt with status, cumulative gas, and logs
		let receipt = Receipt {
			// Success flag was added in `EIP-658: Embedding transaction status code in
			// receipts`.
			status: Eip658Value::Eip658(ctx.result.is_success()),
			cumulative_gas_used: ctx.cumulative_gas_used,
			logs: ctx.result.logs().to_vec(),
		};

		// Wrap in appropriate OpReceipt variant based on transaction type
		match tx_type {
			OpTxType::Legacy => OpReceipt::Legacy(receipt),
			OpTxType::Eip2930 => OpReceipt::Eip2930(receipt),
			OpTxType::Eip1559 => OpReceipt::Eip1559(receipt),
			OpTxType::Eip7702 => OpReceipt::Eip7702(receipt),
			OpTxType::Deposit => {
				// Deposit transactions should not reach this code path in the current
				// implementation. Deposits are handled by the sequencer and included
				// via payload attributes, not executed through the standard execution
				// flow that calls build_receipt.
				unreachable!(
					"Deposit transactions should not be executed through standard execution path"
				)
			}
		}
	}
}