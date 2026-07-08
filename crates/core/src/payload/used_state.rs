//! An EVM inspector that records the state an execution reads and writes.
//!
//! The recorded [`UsedStateTrace`] captures the storage slots, account balances
//! and contract lifecycle changes touched while executing a transaction or a
//! bundle. Builders can use it to detect conflicts between orders and to
//! parallelize block construction across orders that do not touch the same
//! state.

use {
	crate::{
		alloy::{
			consensus::Transaction,
			primitives::{Address, B256, U256},
		},
		reth::{
			evm::revm::{
				Inspector,
				bytecode::opcode,
				context::ContextTr,
				inspector::JournalExt,
				interpreter::{
					CallInputs,
					CallOutcome,
					CreateInputs,
					CreateOutcome,
					Interpreter,
					interpreter_types::Jumps,
				},
			},
			primitives::Recovered,
		},
	},
	std::collections::HashMap,
};

/// Identifies a single storage slot by the account that owns it and the slot
/// key within that account.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotKey {
	pub address: Address,
	pub key: B256,
}

/// A trace of the state read and written while executing an order.
///
/// Limitations: `written_slot_values`, `received_amount` and `sent_amount` are
/// not accurate for a transaction that reverts, because writes performed before
/// the revert are still observed by the interpreter step hook.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsedStateTrace {
	/// First value read from each storage slot.
	pub read_slot_values: HashMap<SlotKey, B256>,
	/// Last value written to each storage slot.
	pub written_slot_values: HashMap<SlotKey, B256>,
	/// First balance read for each account.
	pub read_balances: HashMap<Address, U256>,
	/// Amount of wei received by each account during execution.
	pub received_amount: HashMap<Address, U256>,
	/// Amount of wei sent by each account during execution.
	pub sent_amount: HashMap<Address, U256>,
	/// Contracts created during execution.
	pub created_contracts: Vec<Address>,
	/// Contracts self destructed during execution.
	pub destructed_contracts: Vec<Address>,
}

impl UsedStateTrace {
	/// Merges `other` into `self`, assuming `other` was produced by an execution
	/// that ran after the executions already folded in. The first read and the
	/// last write of each slot are preserved, and transferred amounts are
	/// accumulated.
	pub fn append_trace(&mut self, other: &UsedStateTrace) {
		for (read_slot, read_value) in &other.read_slot_values {
			self
				.read_slot_values
				.entry(read_slot.clone())
				.or_insert(*read_value);
		}

		self
			.written_slot_values
			.extend(other.written_slot_values.clone());

		for (address, balance) in &other.read_balances {
			self.read_balances.entry(*address).or_insert(*balance);
		}

		for (address, received_amount) in &other.received_amount {
			*self.received_amount.entry(*address).or_default() += received_amount;
		}

		for (address, sent_amount) in &other.sent_amount {
			*self.sent_amount.entry(*address).or_default() += sent_amount;
		}

		self
			.created_contracts
			.extend(other.created_contracts.iter().copied());

		for address in &other.destructed_contracts {
			if !self.destructed_contracts.contains(address) {
				self.destructed_contracts.push(*address);
			}
		}
	}

	/// Clears all recorded state.
	pub fn clear(&mut self) {
		self.read_slot_values.clear();
		self.written_slot_values.clear();
		self.read_balances.clear();
		self.received_amount.clear();
		self.sent_amount.clear();
		self.created_contracts.clear();
		self.destructed_contracts.clear();
	}
}

/// A read that must be resolved on the interpreter step following the opcode
/// that requested it, once the result is available on the stack.
#[derive(Debug, Clone, Default)]
enum NextStepAction {
	#[default]
	None,
	ReadSloadKeyResult(B256),
	ReadBalanceResult(Address),
}

/// An EVM [`Inspector`] that records the read and written state of an execution
/// into a [`UsedStateTrace`].
#[derive(Debug)]
pub struct UsedStateInspector<'a> {
	next_step_action: NextStepAction,
	trace: &'a mut UsedStateTrace,
}

impl<'a> UsedStateInspector<'a> {
	/// Creates a new inspector that records into `trace`.
	pub fn new(trace: &'a mut UsedStateTrace) -> Self {
		Self {
			next_step_action: NextStepAction::None,
			trace,
		}
	}

	/// Records the transaction nonce as a read and write of slot zero of the
	/// signer account. A signer account is an externally owned account with no
	/// storage, so modelling the nonce this way lets two transactions from the
	/// same signer be detected as conflicting.
	pub fn use_tx_nonce<T>(&mut self, tx: &Recovered<T>)
	where
		T: Transaction,
	{
		let signer = tx.signer();
		self.trace.read_slot_values.insert(
			SlotKey {
				address: signer,
				key: B256::default(),
			},
			U256::from(tx.nonce()).into(),
		);
		self.trace.written_slot_values.insert(
			SlotKey {
				address: signer,
				key: B256::default(),
			},
			U256::from(tx.nonce().saturating_add(1)).into(),
		);
	}
}

impl<CTX> Inspector<CTX> for UsedStateInspector<'_>
where
	CTX: ContextTr<Journal: JournalExt>,
{
	fn step(&mut self, interpreter: &mut Interpreter, _context: &mut CTX) {
		match core::mem::take(&mut self.next_step_action) {
			NextStepAction::ReadSloadKeyResult(slot) => {
				if let Ok(value) = interpreter.stack.peek(0) {
					let value = B256::from(value.to_be_bytes());
					let key = SlotKey {
						address: interpreter.input.target_address,
						key: slot,
					};
					self.trace.read_slot_values.entry(key).or_insert(value);
				}
			}
			NextStepAction::ReadBalanceResult(addr) => {
				if let Ok(value) = interpreter.stack.peek(0) {
					self.trace.read_balances.entry(addr).or_insert(value);
				}
			}
			NextStepAction::None => {}
		}
		match interpreter.bytecode.opcode() {
			opcode::SLOAD => {
				if let Ok(slot) = interpreter.stack.peek(0) {
					let slot = B256::from(slot.to_be_bytes());
					self.next_step_action = NextStepAction::ReadSloadKeyResult(slot);
				}
			}
			opcode::SSTORE => {
				if let (Ok(slot), Ok(value)) =
					(interpreter.stack.peek(0), interpreter.stack.peek(1))
				{
					let written_value = B256::from(value.to_be_bytes());
					let key = SlotKey {
						address: interpreter.input.target_address,
						key: B256::from(slot.to_be_bytes()),
					};
					// If we write the value we first read then there is no net
					// write, so drop any previously recorded write to this slot.
					if self.trace.read_slot_values.get(&key) == Some(&written_value) {
						self.trace.written_slot_values.remove(&key);
						return;
					}
					self.trace.written_slot_values.insert(key, written_value);
				}
			}
			opcode::BALANCE => {
				if let Ok(addr) = interpreter.stack.peek(0) {
					let addr = Address::from_word(B256::from(addr.to_be_bytes()));
					self.next_step_action = NextStepAction::ReadBalanceResult(addr);
				}
			}
			opcode::SELFBALANCE => {
				let addr = interpreter.input.target_address;
				self.next_step_action = NextStepAction::ReadBalanceResult(addr);
			}
			_ => {}
		}
	}

	fn call(
		&mut self,
		_context: &mut CTX,
		inputs: &mut CallInputs,
	) -> Option<CallOutcome> {
		if let Some(transfer_value) = inputs.transfer_value()
			&& !transfer_value.is_zero()
		{
			*self
				.trace
				.sent_amount
				.entry(inputs.transfer_from())
				.or_default() += transfer_value;
			*self
				.trace
				.received_amount
				.entry(inputs.transfer_to())
				.or_default() += transfer_value;
		}
		None
	}

	fn create_end(
		&mut self,
		_context: &mut CTX,
		_inputs: &CreateInputs,
		outcome: &mut CreateOutcome,
	) {
		if let Some(addr) = outcome.address {
			self.trace.created_contracts.push(addr);
		}
	}

	fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
		// selfdestruct can be reported more than once for the same contract
		// during execution, so only record the first occurrence.
		if self.trace.destructed_contracts.contains(&contract) {
			return;
		}
		self.trace.destructed_contracts.push(contract);
		if !value.is_zero() {
			*self.trace.sent_amount.entry(contract).or_default() += value;
			*self.trace.received_amount.entry(target).or_default() += value;
		}
	}
}
