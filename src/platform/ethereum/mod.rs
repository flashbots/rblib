use {
	super::*,
	crate::{
		alloy::{
			consensus::BlockHeader,
			evm::revm::database::State,
			primitives::U256,
		},
		reth::{
			chainspec::EthChainSpec,
			errors::ConsensusError,
			ethereum::{
				EthPrimitives,
				TransactionSigned,
				chainspec::EthereumHardforks,
				consensus::validation::MAX_RLP_BLOCK_SIZE,
				evm::{EthEvmConfig, revm::database::StateProviderDatabase},
				node::EthereumNode,
				primitives::transaction::error::InvalidTransactionError,
			},
			evm::{
				ConfigureEvm,
				Evm,
				NextBlockEnvAttributes,
				block::{BlockExecutionError, BlockValidationError},
				execute::{BlockBuilder, BlockBuilderOutcome},
			},
			payload::{
				BlobSidecars,
				EthBuiltPayload,
				EthPayloadBuilderAttributes,
				PayloadBuilderAttributes,
				builder::{
					BuildArguments,
					BuildOutcome,
					EthereumBuilderConfig,
					PayloadConfig,
					is_better_payload,
				},
			},
			revm::{
				cached::CachedReads,
				cancelled::CancelOnDrop,
				context::Block,
				primitives::alloy_primitives::private::alloy_rlp::Encodable,
			},
			rpc::types::TransactionTrait,
			transaction_pool::{
				error::{Eip4844PoolTransactionError, InvalidPoolTransactionError},
				noop::NoopTransactionPool,
				*,
			},
		},
	},
	limits::EthereumDefaultLimits,
	pool::FixedTransactions,
	serde::{Deserialize, Serialize},
	std::sync::Arc,
};

mod limits;
mod pool;

/// Platform definition for ethereum mainnet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ethereum;

impl Platform for Ethereum {
	type Bundle = FlashbotsBundle<Self>;
	type DefaultLimits = EthereumDefaultLimits;
	type EvmConfig = EthEvmConfig;
	type NodeTypes = EthereumNode;
	type PooledTransaction = EthPooledTransaction;

	fn evm_config<P>(chainspec: Arc<types::ChainSpec<Self>>) -> Self::EvmConfig {
		EthEvmConfig::new(chainspec)
	}

	fn next_block_environment_context<P>(
		_: &types::ChainSpec<Self>,
		parent: &types::Header<Self>,
		attributes: &types::PayloadBuilderAttributes<Self>,
	) -> types::NextBlockEnvContext<Self> {
		NextBlockEnvAttributes {
			timestamp: attributes.timestamp,
			suggested_fee_recipient: attributes.suggested_fee_recipient,
			prev_randao: attributes.prev_randao,
			gas_limit: EthereumBuilderConfig::new().gas_limit(parent.gas_limit()),
			parent_beacon_block_root: attributes.parent_beacon_block_root,
			withdrawals: Some(attributes.withdrawals.clone()),
		}
	}

	fn build_payload<P>(
		payload: Checkpoint<P>,
		provider: &dyn StateProvider,
	) -> Result<types::BuiltPayload<P>, PayloadBuilderError>
	where
		P: traits::PlatformExecBounds<Self>,
	{
		let evm_config = payload.block().evm_config().clone();
		let chain_spec = payload.block().chainspec();
		let payload_config = PayloadConfig {
			parent_header: Arc::new(payload.block().parent().clone()),
			attributes: payload.block().attributes().clone(),
		};

		let build_args = BuildArguments::<
			types::PayloadBuilderAttributes<Self>,
			types::BuiltPayload<Self>,
		>::new(
			CachedReads::default(),
			payload_config,
			CancelOnDrop::default(),
			None,
		);

		let builder_config = EthereumBuilderConfig::new();
		let transactions = payload.history().transactions().cloned().collect();
		let transactions = Box::new(FixedTransactions::<Self>::new(transactions));

		default_ethereum_payload(
			evm_config,
			chain_spec,
			provider,
			NoopTransactionPool::default(),
			&builder_config,
			build_args,
			|_| {
				transactions
					as Box<
						dyn BestTransactions<
							Item = Arc<ValidPoolTransaction<Self::PooledTransaction>>,
						>,
					>
			},
		)?
		.into_payload()
		.ok_or_else(|| PayloadBuilderError::MissingPayload)
	}
}

impl PlatformWithRpcTypes for Ethereum {
	type RpcTypes = alloy::network::Ethereum;
}

/// Ethereum payload builder code from [`default_ethereum_payload`] adapted to
/// use the given `StateProvider` and chainspec
///
/// Constructs an Ethereum transaction payload using the best transactions
///
/// Given build arguments including latest state provider, best transactions,
/// and configuration, this function creates a transaction payload.
/// Returns a result indicating success with the payload or an error in case of
/// failure.
///
/// # Panics
/// see [`default_ethereum_payload`]
///
/// [`default_ethereum_payload`]: reth_ethereum_payload_builder::default_ethereum_payload
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
#[inline]
pub fn default_ethereum_payload<EvmConfig, Pool, F>(
	evm_config: EvmConfig,
	chain_spec: &Arc<types::ChainSpec<Ethereum>>,
	state_provider: &dyn StateProvider,
	pool: Pool,
	builder_config: &EthereumBuilderConfig,
	args: BuildArguments<EthPayloadBuilderAttributes, EthBuiltPayload>,
	best_txs: F,
) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>
where
	EvmConfig: ConfigureEvm<
			Primitives = EthPrimitives,
			NextBlockEnvCtx = NextBlockEnvAttributes,
		>,
	Pool: TransactionPool<
		Transaction: PoolTransaction<Consensus = TransactionSigned>,
	>,
	F: FnOnce(BestTransactionsAttributes) -> BestTransactionsFor<Pool>,
{
	let BuildArguments {
		mut cached_reads,
		config,
		cancel,
		best_payload,
	} = args;
	let PayloadConfig {
		parent_header,
		attributes,
	} = config;

	let state = StateProviderDatabase::new(&state_provider);
	let mut db = State::builder()
		.with_database(cached_reads.as_db_mut(state))
		.with_bundle_update()
		.build();

	let mut builder = evm_config
		.builder_for_next_block(&mut db, &parent_header, NextBlockEnvAttributes {
			timestamp: attributes.timestamp(),
			suggested_fee_recipient: attributes.suggested_fee_recipient(),
			prev_randao: attributes.prev_randao(),
			gas_limit: builder_config.gas_limit(parent_header.gas_limit),
			parent_beacon_block_root: attributes.parent_beacon_block_root(),
			withdrawals: Some(attributes.withdrawals().clone()),
		})
		.map_err(PayloadBuilderError::other)?;

	let mut cumulative_gas_used = 0;
	let block_gas_limit: u64 = builder.evm_mut().block().gas_limit;
	let base_fee = builder.evm_mut().block().basefee;

	let mut best_txs = best_txs(BestTransactionsAttributes::new(
		base_fee,
		builder
			.evm_mut()
			.block()
			.blob_gasprice()
			.map(|gasprice| gasprice as u64),
	));
	let mut total_fees = U256::ZERO;

	builder
		.apply_pre_execution_changes()
		.map_err(|err| PayloadBuilderError::Internal(err.into()))?;

	// initialize empty blob sidecars at first. If cancun is active then this will
	// be populated by blob sidecars if any.
	let mut blob_sidecars = BlobSidecars::Empty;

	let mut block_blob_count = 0;
	let mut block_transactions_rlp_length = 0;

	let blob_params = chain_spec.blob_params_at_timestamp(attributes.timestamp);
	let max_blob_count = blob_params
		.as_ref()
		.map(|params| params.max_blob_count)
		.unwrap_or_default();

	let is_osaka = chain_spec.is_osaka_active_at_timestamp(attributes.timestamp);

	while let Some(pool_tx) = best_txs.next() {
		// ensure we still have capacity for this transaction
		if cumulative_gas_used + pool_tx.gas_limit() > block_gas_limit {
			// we can't fit this transaction into the block, so we need to mark it as
			// invalid which also removes all dependent transaction from the
			// iterator before we can continue
			best_txs.mark_invalid(
				&pool_tx,
				InvalidPoolTransactionError::ExceedsGasLimit(
					pool_tx.gas_limit(),
					block_gas_limit,
				),
			);
			continue;
		}

		// check if the job was cancelled, if so we can exit early
		if cancel.is_cancelled() {
			return Ok(BuildOutcome::Cancelled);
		}

		// convert tx to a signed transaction
		let tx = pool_tx.to_consensus();

		let estimated_block_size_with_tx = block_transactions_rlp_length
			+ tx.inner().length()
			+ attributes.withdrawals().length()
			+ 1024; // 1Kb of overhead for the block header

		if is_osaka && estimated_block_size_with_tx > MAX_RLP_BLOCK_SIZE {
			best_txs.mark_invalid(
				&pool_tx,
				InvalidPoolTransactionError::OversizedData(
					estimated_block_size_with_tx,
					MAX_RLP_BLOCK_SIZE,
				),
			);
			continue;
		}

		// There's only limited amount of blob space available per block, so we need
		// to check if the EIP-4844 can still fit in the block
		let mut blob_tx_sidecar = None;
		if let Some(blob_tx) = tx.as_eip4844() {
			let tx_blob_count = blob_tx.tx().blob_versioned_hashes.len() as u64;

			if block_blob_count + tx_blob_count > max_blob_count {
				// we can't fit this _blob_ transaction into the block, so we mark it as
				// invalid, which removes its dependent transactions from
				// the iterator. This is similar to the gas limit condition
				// for regular transactions above.
				best_txs.mark_invalid(
					&pool_tx,
					InvalidPoolTransactionError::Eip4844(
						Eip4844PoolTransactionError::TooManyEip4844Blobs {
							have: block_blob_count + tx_blob_count,
							permitted: max_blob_count,
						},
					),
				);
				continue;
			}

			let blob_sidecar_result = 'sidecar: {
				let Some(sidecar) = pool
					.get_blob(*tx.hash())
					.map_err(PayloadBuilderError::other)?
				else {
					break 'sidecar Err(
						Eip4844PoolTransactionError::MissingEip4844BlobSidecar,
					);
				};

				if is_osaka {
					if sidecar.is_eip7594() {
						Ok(sidecar)
					} else {
						Err(Eip4844PoolTransactionError::UnexpectedEip4844SidecarAfterOsaka)
					}
				} else if sidecar.is_eip4844() {
					Ok(sidecar)
				} else {
					Err(Eip4844PoolTransactionError::UnexpectedEip7594SidecarBeforeOsaka)
				}
			};

			blob_tx_sidecar = match blob_sidecar_result {
				Ok(sidecar) => Some(sidecar),
				Err(error) => {
					best_txs.mark_invalid(
						&pool_tx,
						InvalidPoolTransactionError::Eip4844(error),
					);
					continue;
				}
			};
		}

		let gas_used = match builder.execute_transaction(tx.clone()) {
			Ok(gas_used) => gas_used,
			Err(BlockExecutionError::Validation(
				BlockValidationError::InvalidTx { error, .. },
			)) => {
				if error.is_nonce_too_low() {
					// if the nonce is too low, we can skip this transaction
				} else {
					// if the transaction is invalid, we can skip it and all of its
					// descendants
					best_txs.mark_invalid(
						&pool_tx,
						InvalidPoolTransactionError::Consensus(
							InvalidTransactionError::TxTypeNotSupported,
						),
					);
				}
				continue;
			}
			// this is an error that we should treat as fatal for this attempt
			Err(err) => return Err(PayloadBuilderError::evm(err)),
		};

		// add to the total blob gas used if the transaction successfully executed
		if let Some(blob_tx) = tx.as_eip4844() {
			block_blob_count += blob_tx.tx().blob_versioned_hashes.len() as u64;

			// if we've reached the max blob count, we can skip blob txs entirely
			if block_blob_count == max_blob_count {
				best_txs.skip_blobs();
			}
		}

		block_transactions_rlp_length += tx.inner().length();

		// update and add to total fees
		let miner_fee = tx
			.effective_tip_per_gas(base_fee)
			.expect("fee is always valid; execution succeeded");
		total_fees += U256::from(miner_fee) * U256::from(gas_used);
		cumulative_gas_used += gas_used;

		// Add blob tx sidecar to the payload.
		if let Some(sidecar) = blob_tx_sidecar {
			blob_sidecars.push_sidecar_variant(sidecar.as_ref().clone());
		}
	}

	// check if we have a better block
	if !is_better_payload(best_payload.as_ref(), total_fees) {
		// Release db
		drop(builder);
		// can skip building the block
		return Ok(BuildOutcome::Aborted {
			fees: total_fees,
			cached_reads,
		});
	}

	let BlockBuilderOutcome {
		execution_result,
		block,
		..
	} = builder.finish(state_provider)?;

	let requests = chain_spec
		.is_prague_active_at_timestamp(attributes.timestamp)
		.then_some(execution_result.requests);

	let sealed_block = Arc::new(block.sealed_block().clone());

	if is_osaka && sealed_block.rlp_length() > MAX_RLP_BLOCK_SIZE {
		return Err(PayloadBuilderError::other(ConsensusError::BlockTooLarge {
			rlp_length: sealed_block.rlp_length(),
			max_rlp_length: MAX_RLP_BLOCK_SIZE,
		}));
	}

	let payload = EthBuiltPayload::new(attributes.id, sealed_block, total_fees, requests)
		// add blob sidecars from the executed txs
		.with_sidecars(blob_sidecars);

	Ok(BuildOutcome::Better {
		payload,
		cached_reads,
	})
}
