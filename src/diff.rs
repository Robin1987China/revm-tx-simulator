//! Purely-functional diff helpers: turn a revm post-state plus a pre-state
//! lookup into human-readable balance and storage changes.
//!
//! These functions do no I/O so they can be unit-tested directly.

use std::collections::BTreeMap;

use revm::primitives::{Address, U256};
use revm::state::EvmState;

use crate::hexutil;

/// ETH balance change for one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceChange {
    pub address: Address,
    pub before: U256,
    pub after: U256,
}

impl BalanceChange {
    pub fn delta(&self) -> U256 {
        self.after.abs_diff(self.before)
    }
    pub fn is_increase(&self) -> bool {
        self.after > self.before
    }
}

/// One storage slot that actually changed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageChange {
    pub address: Address,
    pub slot: U256,
    pub before: U256,
    pub after: U256,
}

/// Diff ETH balances from the post-state against a pre-state snapshot.
///
/// `pre` maps address -> pre balance (0 when absent). Only accounts whose
/// balance actually moved are returned, sorted by address for stable output.
pub fn diff_balances(pre: &BTreeMap<Address, U256>, post: &EvmState) -> Vec<BalanceChange> {
    let mut out = Vec::new();
    for (address, account) in post.iter() {
        let before = pre.get(address).copied().unwrap_or(U256::ZERO);
        let after = account.info.balance;
        if before != after {
            out.push(BalanceChange { address: *address, before, after });
        }
    }
    out.sort_by_key(|c| c.address);
    out
}

/// Collect every storage slot whose present value differs from its original.
pub fn diff_storage(post: &EvmState) -> Vec<StorageChange> {
    let mut out = Vec::new();
    for (address, account) in post.iter() {
        for (slot, value) in account.storage.iter() {
            if value.present_value != value.original_value {
                out.push(StorageChange {
                    address: *address,
                    slot: *slot,
                    before: value.original_value,
                    after: value.present_value,
                });
            }
        }
    }
    out.sort_by(|a, b| a.address.cmp(&b.address).then(a.slot.cmp(&b.slot)));
    out
}

/// One account whose nonce advanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonceChange {
    pub address: Address,
    pub before: u64,
    pub after: u64,
}

/// Diff nonces from the post-state against a pre-state snapshot.
pub fn diff_nonces(pre: &BTreeMap<Address, u64>, post: &EvmState) -> Vec<NonceChange> {
    let mut out = Vec::new();
    for (address, account) in post.iter() {
        let before = pre.get(address).copied().unwrap_or(0);
        let after = account.info.nonce;
        if before != after {
            out.push(NonceChange { address: *address, before, after });
        }
    }
    out.sort_by_key(|c| c.address);
    out
}

/// Render a balance change as JSON.
pub fn balance_change_json(c: &BalanceChange) -> serde_json::Value {
    serde_json::json!({
        "address": format!("{:?}", c.address),
        "before_wei": c.before.to_string(),
        "after_wei": c.after.to_string(),
        "delta_wei": c.delta().to_string(),
        "direction": if c.is_increase() { "in" } else { "out" },
        "before_hex": hexutil::hex_u256(c.before),
        "after_hex": hexutil::hex_u256(c.after),
    })
}

/// Render a storage change as JSON.
pub fn storage_change_json(c: &StorageChange) -> serde_json::Value {
    serde_json::json!({
        "address": format!("{:?}", c.address),
        "slot": hexutil::hex_u256(c.slot),
        "before": hexutil::hex_u256(c.before),
        "after": hexutil::hex_u256(c.after),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use revm::state::{Account, EvmStorageSlot, TransactionId};

    fn account_with(balance: u64) -> Account {
        let mut a = Account::default();
        a.info.balance = U256::from(balance);
        a
    }

    #[test]
    fn balance_diff_skips_unchanged_and_reports_direction() {
        let a1 = Address::with_last_byte(1);
        let a2 = Address::with_last_byte(2);
        let mut pre = BTreeMap::new();
        pre.insert(a1, U256::from(100));
        pre.insert(a2, U256::from(7));

        let mut post = EvmState::default();
        post.insert(a1, account_with(90)); // -10
        post.insert(a2, account_with(7)); // unchanged -> omitted

        let d = diff_balances(&pre, &post);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].address, a1);
        assert_eq!(d[0].delta(), U256::from(10));
        assert!(!d[0].is_increase());
    }

    #[test]
    fn storage_diff_only_reports_real_changes() {
        let a = Address::with_last_byte(3);
        let mut acc = Account::default();
        acc.storage.insert(
            U256::from(1),
            EvmStorageSlot::new_changed(U256::ZERO, U256::from(5), TransactionId::ZERO),
        );
        acc.storage.insert(
            U256::from(2),
            EvmStorageSlot::new(U256::from(9), TransactionId::ZERO),
        );
        let mut post = EvmState::default();
        post.insert(a, acc);

        let d = diff_storage(&post);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].slot, U256::from(1));
        assert_eq!(d[0].before, U256::ZERO);
        assert_eq!(d[0].after, U256::from(5));
    }
}
