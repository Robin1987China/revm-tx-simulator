//! Integration tests that run the *actual* revm EVM against an in-memory
//! database. No network access is required.
//!
//! The headline test is `calldata_change_changes_storage_diff`: the same
//! transaction with one byte of calldata edited must produce a different
//! storage diff. That is the discriminating-power check for the diff logic.

use std::collections::BTreeMap;

use revm::context::TxEnv;
use revm::database::EmptyDB;
use revm::database::CacheDB;
use revm::primitives::{hardfork::SpecId, Address, Bytes, TxKind, U256};
use revm::state::{AccountInfo, Bytecode};

use revm_txsim::diff;
use revm_txsim::sim;

const CALLER: Address = Address::new([0x11; 20]);
const TARGET: Address = Address::new([0x22; 20]);
const RECIPIENT: Address = Address::new([0x33; 20]);

/// Contract that does `sstore(1, calldataload(0))` then stops.
///
///   60 00  CALLDATALOAD
///   60 01  PUSH1 0x01
///   55     SSTORE
///   00     STOP
fn store_calldata_contract() -> Bytecode {
    Bytecode::new_legacy(Bytes::from(vec![0x60, 0x00, 0x35, 0x60, 0x01, 0x55, 0x00]))
}

fn base_db() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::<EmptyDB>::default();

    let mut caller = AccountInfo::default();
    caller.balance = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
    db.insert_account_info(CALLER, caller);

    let mut target = AccountInfo::default();
    target.code = Some(store_calldata_contract());
    target.code_hash = store_calldata_contract().hash_slow();
    db.insert_account_info(TARGET, target);

    let mut recipient = AccountInfo::default();
    recipient.balance = U256::from(1_000u64);
    db.insert_account_info(RECIPIENT, recipient);

    db
}

fn call_target(data: [u8; 32]) -> TxEnv {
    TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(TARGET))
        .value(U256::ZERO)
        .data(Bytes::from(data.to_vec()))
        .gas_limit(100_000)
        .nonce(0)
        .chain_id(Some(1))
        .gas_price(0)
        .build()
        .unwrap()
}

fn run(data: [u8; 32]) -> sim::Simulation {
    let env = revm::context::BlockEnv {
        number: U256::from(1_000_000u64),
        timestamp: U256::from(1_700_000_000u64),
        gas_limit: 30_000_000,
        basefee: 0,
        prevrandao: Some(revm::primitives::B256::ZERO),
        ..Default::default()
    };
    sim::simulate(base_db(), env, 1, SpecId::PRAGUE, call_target(data)).expect("simulation")
}

#[test]
fn call_writes_calldata_into_slot_one() {
    let mut input = [0u8; 32];
    input[31] = 7; // calldata word = 7
    let sim = run(input);
    assert!(sim.is_success(), "tx should succeed, got {:?}", sim.result);

    let changes = diff::diff_storage(&sim.state);
    let hit = changes
        .iter()
        .find(|c| c.address == TARGET && c.slot == U256::from(1))
        .expect("slot 1 of TARGET should have changed");
    assert_eq!(hit.before, U256::ZERO);
    assert_eq!(hit.after, U256::from(7));
}

/// THE discriminating-power check: edit one byte of calldata, output must move.
#[test]
fn calldata_change_changes_storage_diff() {
    let mut a = [0u8; 32];
    a[31] = 7;
    let mut b = [0u8; 32];
    b[31] = 8; // one byte different

    let sim_a = run(a);
    let sim_b = run(b);

    let diff_a = diff::diff_storage(&sim_a.state);
    let diff_b = diff::diff_storage(&sim_b.state);

    assert_ne!(diff_a, diff_b, "editing calldata must change the storage diff");

    let slot_a = diff_a.iter().find(|c| c.slot == U256::from(1)).unwrap();
    let slot_b = diff_b.iter().find(|c| c.slot == U256::from(1)).unwrap();
    assert_eq!(slot_a.after, U256::from(7));
    assert_eq!(slot_b.after, U256::from(8));
}

#[test]
fn calldata_change_changes_emitted_gas_or_output_is_observable_in_diff() {
    // A second angle on the same property: identical calldata => identical diff.
    let mut a = [0u8; 32];
    a[31] = 42;
    let d1 = diff::diff_storage(&run(a).state);
    let d2 = diff::diff_storage(&run(a).state);
    assert_eq!(d1, d2, "same input must be deterministic");
}

#[test]
fn plain_value_transfer_moves_exact_balance_with_zero_gas_price() {
    let env = revm::context::BlockEnv {
        number: U256::from(1_000_000u64),
        timestamp: U256::from(1_700_000_000u64),
        gas_limit: 30_000_000,
        basefee: 0,
        prevrandao: Some(revm::primitives::B256::ZERO),
        ..Default::default()
    };
    let value = U256::from(500_000_000_000_000_000u128); // 0.5 ETH
    let tx = TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(RECIPIENT))
        .value(value)
        .data(Bytes::new())
        .gas_limit(21_000)
        .nonce(0)
        .chain_id(Some(1))
        .gas_price(0)
        .build()
        .unwrap();

    let sim = sim::simulate(base_db(), env, 1, SpecId::PRAGUE, tx).expect("simulation");
    assert!(sim.is_success());

    let changes = diff::diff_balances(&sim.pre_balances, &sim.state);
    let sender = changes.iter().find(|c| c.address == CALLER).expect("sender changed");
    let receiver = changes.iter().find(|c| c.address == RECIPIENT).expect("receiver changed");

    // gas_price is 0 => exactly `value` moves, no fee noise.
    assert_eq!(sender.delta(), value);
    assert!(!sender.is_increase());
    assert_eq!(receiver.delta(), value);
    assert!(receiver.is_increase());

    // nonces: caller advanced, recipient untouched.
    let nonces = diff::diff_nonces(&sim.pre_nonces, &sim.state);
    assert_eq!(nonces.len(), 1);
    assert_eq!(nonces[0].address, CALLER);
    assert_eq!(nonces[0].after, 1);
}

#[test]
fn block_env_and_spec_helpers_agree() {
    // sanity: default in-memory BlockEnv is usable and specs parse.
    let env = revm::context::BlockEnv::default();
    assert_eq!(env.gas_limit, u64::MAX);
    assert!(sim::spec_from_name("cancun").is_ok());
    assert_eq!(sim::spec_for_block(20_000_000), SpecId::CANCUN);
}

#[test]
fn empty_pre_state_means_zero_before() {
    let mut pre = BTreeMap::new();
    pre.insert(CALLER, U256::from(5));
    let mut post = revm::state::EvmState::default();
    let mut acc = revm::state::Account::default();
    acc.info.balance = U256::from(9);
    post.insert(RECIPIENT, acc); // not in `pre`
    let d = diff::diff_balances(&pre, &post);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].before, U256::ZERO);
    assert_eq!(d[0].after, U256::from(9));
}
