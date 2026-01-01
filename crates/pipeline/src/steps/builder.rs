use {
	crate::{alloy, prelude::*, reth},
	alloy::{
		eips::eip7623::TOTAL_COST_FLOOR_PER_TOKEN,
		network::TransactionBuilder,
		primitives::{Address, Bytes},
		signers::local::PrivateKeySigner,
	},
	reth::ethereum::chainspec::EthChainSpec,
	std::sync::Arc,
	tracing::warn,
};

/// This step is used as an epilogue of a payload building pipeline that adds a
/// builder transaction to the block with a message.
///
/// Platforms that use this step should implement the `PlatformWithRpcTypes`
/// trait because this step constructs a platform-specific transaction.
///
/// It is up to the builder implementation to ensure that the account signing
/// the builder transaction has enough funds to pay for the transaction gas.
///
/// The transaction gas is proportional to the length of the message.
///
/// This step also provides its own limits factory that inherits the limits of
/// the enclosing (or default) limits factory, but subtracts the gas limit for
/// the builder transaction from the gas limit of the block.
pub struct BuilderEpilogue<P: PlatformWithRpcTypes> {
	signer: PrivateKeySigner,
	required: bool,
	message_fn: MessageFn<P>,
}

impl<P: PlatformWithRpcTypes> BuilderEpilogue<P> {
	/// Creates a new `BuilderEpilogue` step with the given tx signer.
	pub fn with_signer(signer: PrivateKeySigner) -> Self {
		Self {
			signer,
			required: false,
			message_fn: Arc::new(|block: &BlockContext<P>| {
				format!("flashbots rblib block #{}", block.number())
			}) as MessageFn<P>,
		}
	}

	/// Specifies custom logic to generate the message for the epilogue
	/// transaction.
	#[must_use]
	pub fn with_message<F>(mut self, message_fn: F) -> Self
	where
		F: Fn(&BlockContext<P>) -> String + Send + Sync + 'static,
	{
		self.message_fn = Arc::new(message_fn);
		self
	}

	/// If the builder epilogue is required then the payload will fail if the
	/// transaction is not included, otherwise the epilogue transaction will be
	/// treated as a nice to have optional.
	#[must_use]
	pub fn required(mut self) -> Self {
		self.required = true;
		self
	}

	/// Creates an instance of `LimitsFactory` that will account for the epilogue
	/// transaction when calculating the limits for the block and inherit all
	/// limits from either the enclosing limits or the default limits for the
	/// platform.
	pub fn limiter(&self) -> impl ScopedLimits<P> {
		LimitsMinusEpilogue {
			message_fn: Arc::clone(&self.message_fn),
		}
	}
}

impl<P: PlatformWithRpcTypes> Step<P> for BuilderEpilogue<P> {
	async fn step(
		self: Arc<Self>,
		payload: Checkpoint<P>,
		ctx: StepContext<P>,
	) -> ControlFlow<P> {
		// if the epilogue transaction cannot be built or applied to the payload,
		// we either fail the pipeline if it is marked as required or return the
		// payload as is without the epilogue transaction.
		macro_rules! try_ {
			($expr:expr) => {
				match $expr {
					Ok(value) => value,
					Err(error) => {
						if self.required {
							return ControlFlow::Fail(PayloadBuilderError::Other(
								error.into(),
							));
						}
						warn!(
							"Failed to build builder epilogue transaction, skipping: \
							 {error:?}"
						);
						return ControlFlow::Ok(payload);
					}
				}
			};
		}

		let signer_nonce = try_!(payload.nonce_of(self.signer.address()));
		let message: Bytes = (self.message_fn)(ctx.block()).into_bytes().into();
		let gas_estimate = estimate_gas_for_tx(&message);

		let tx_params = types::TransactionRequest::<P>::default()
			.with_chain_id(ctx.block().chainspec().chain_id())
			.with_nonce(signer_nonce)
			.with_gas_limit(gas_estimate)
			.with_max_fee_per_gas(ctx.block().base_fee().into())
			.with_max_priority_fee_per_gas(0)
			.with_to(Address::ZERO)
			.with_input(message);

		let signed_tx = try_!(build_signed::<P>(tx_params, &self.signer));
		let new_payload = try_!(payload.apply(signed_tx));

		ControlFlow::Ok(new_payload)
	}
}

type MessageFn<P: Platform> =
	Arc<dyn Fn(&BlockContext<P>) -> String + Send + Sync + 'static>;

/// A `LimitsFactory` that subtracts the gas limit for the epilogue transaction
/// from the gas limit of the block. It inherits the limits from the enclosing
/// limits factory or the default limits for the platform.
struct LimitsMinusEpilogue<P: Platform> {
	message_fn: MessageFn<P>,
}

impl<P: Platform> ScopedLimits<P> for LimitsMinusEpilogue<P> {
	fn create(
		&self,
		payload: &Checkpoint<P>,
		enclosing: &Limits<P>,
	) -> Limits<P> {
		// calculate the gas usage for the epilogue transaction
		let message = (self.message_fn)(payload.block());
		let gas_estimate = estimate_gas_for_tx(message.as_bytes());
		Scaled::default().gas(Minus(gas_estimate)).from(enclosing)
	}
}

fn estimate_gas_for_tx(message: &[u8]) -> u64 {
	// Count zero and non-zero bytes
	let (zero_bytes, nonzero_bytes) =
		message.iter().fold((0, 0), |(zeros, nonzeros), &byte| {
			if byte == 0 {
				(zeros + 1, nonzeros)
			} else {
				(zeros, nonzeros + 1)
			}
		});

	// Calculate gas cost (4 gas per zero byte, 16 gas per non-zero byte)
	let zero_cost = zero_bytes * 4;
	let nonzero_cost = nonzero_bytes * 16;

	// Tx gas should be not less than floor gas https://eips.ethereum.org/EIPS/eip-7623
	let tokens_in_calldata = zero_bytes + nonzero_bytes * 4;
	let floor_gas = 21_000 + tokens_in_calldata * TOTAL_COST_FLOOR_PER_TOKEN;

	std::cmp::max(zero_cost + nonzero_cost + 21_000, floor_gas)
}

#[cfg(test)]
mod tests {
	use {
		super::*,
		crate::test_utils::*,
		alloy::{consensus::Transaction, primitives::U256},
	};

	#[rblib_test(Ethereum, Optimism)]
	async fn empty_payload_has_builder_tx<P: TestablePlatform>() {
		let step = OneStep::<P>::new(BuilderEpilogue::<P>::with_signer(
			FundedAccounts::signer(0),
		));

		let output = step.run().await;
		let Ok(ControlFlow::Ok(payload)) = output else {
			panic!("Expected Ok payload, got {output:?}");
		};

		let history = payload.history();

		assert_eq!(history.len(), 2); // init checkpoint + builder tx
		assert_eq!(history.transactions().count(), 1);

		let builder_tx = history.transactions().next().unwrap();

		assert_eq!(builder_tx.signer(), FundedAccounts::signer(0).address());
		assert_eq!(builder_tx.to(), Some(Address::ZERO));
		assert_eq!(builder_tx.input(), "flashbots rblib block #1".as_bytes());
		assert_eq!(builder_tx.value(), U256::ZERO);
	}

	/// Ensure that the builder epilogue step correctly retrieves the current
	/// nonce of the signer. In this test we will have a payload with existing
	/// transactions signed by the same signer, and the builder epilogue should
	/// use the next nonce.
	#[rblib_test(Ethereum, Optimism)]
	async fn builder_signer_nonce_is_correct<P: TestablePlatform>() {
		let step = OneStep::<P>::new(BuilderEpilogue::<P>::with_signer(
			FundedAccounts::signer(0),
		))
		.with_payload_tx(|tx| tx.transfer().with_funded_signer(0).with_nonce(0))
		.with_payload_tx(|tx| tx.transfer().with_funded_signer(0).with_nonce(1));

		let output = step.run().await;
		let Ok(ControlFlow::Ok(payload)) = output else {
			panic!("Expected Ok payload, got {output:?}");
		};

		let history = payload.history();

		assert_eq!(history.len(), 4); // init checkpoint + builder tx + 2 user txs
		assert_eq!(history.transactions().count(), 3);

		// builder tx should be the last one
		let builder_tx = history.transactions().last().unwrap();
		assert_eq!(builder_tx.signer(), FundedAccounts::signer(0).address());
		assert_eq!(builder_tx.nonce(), 2); // next nonce after the two user transactions
		assert_eq!(builder_tx.to(), Some(Address::ZERO));
		assert_eq!(builder_tx.input(), "flashbots rblib block #1".as_bytes());
		assert_eq!(builder_tx.value(), U256::ZERO);
	}
}
