//! Platform-specific receipt building
//!
//! This module provides a trait for building platform-specific transaction
//! receipts from execution results. Different platforms (Ethereum, Optimism,
//! etc.) have different receipt formats and requirements.

use crate::{prelude::*, reth};

/// Context for building a receipt from transaction execution.
#[derive(Debug)]
pub struct ReceiptBuilderCtx<'a, P: Platform> {
	/// The executed transaction
	pub tx: &'a reth::primitives::Recovered<types::Transaction<P>>,
	/// Result of transaction execution
	pub result: &'a types::TransactionExecutionResult<P>,
	/// Cumulative gas used up to and including this transaction
	pub cumulative_gas_used: u64,
}

/// Trait for building platform-specific receipts from execution results.
///
/// This trait allows each platform to define how transaction receipts should
/// be constructed from execution results. Different platforms may have
/// different receipt formats (e.g., Ethereum vs Optimism deposit receipts).
pub trait ReceiptBuilder<P: Platform> {
	/// Builds a receipt from the execution context.
	fn build_receipt(ctx: ReceiptBuilderCtx<'_, P>) -> types::Receipt<P>;
}