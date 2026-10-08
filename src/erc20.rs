//! ERC-20 `Transfer` event decoding and net-balance aggregation.
//!
//! Balance deltas are derived from the logs the *simulated* transaction
//! produced, not from guessing `balanceOf` storage slots. That makes it work for
//! any standard ERC-20 regardless of its storage layout.

use std::collections::BTreeMap;

use revm::primitives::{keccak256, Address, B256, Log, U256};

/// A decoded `Transfer(address indexed from, address indexed to, uint256 value)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub value: U256,
}

/// keccak256("Transfer(address,address,uint256)").
pub fn transfer_topic0() -> B256 {
    keccak256(b"Transfer(address,address,uint256)")
}

/// Try to decode a single log as an ERC-20 `Transfer`.
///
/// Returns `None` for anything that does not match the exact signature
/// (topic count, topic0, value word length).
pub fn decode_transfer(log: &Log) -> Option<Transfer> {
    let topics = log.data.topics();
    if topics.len() != 3 {
        return None;
    }
    if topics[0] != transfer_topic0() {
        return None;
    }
    if log.data.data.len() < 32 {
        return None;
    }
    // Addresses are right-aligned in a 32-byte topic.
    let from = Address::from_slice(&topics[1].as_slice()[12..32]);
    let to = Address::from_slice(&topics[2].as_slice()[12..32]);
    let mut word = [0u8; 32];
    word.copy_from_slice(&log.data.data[..32]);
    let value = U256::from_be_bytes(word);
    Some(Transfer { token: log.address, from, to, value })
}

/// Decode every `Transfer` in `logs`, preserving order.
pub fn decode_transfers(logs: &[Log]) -> Vec<Transfer> {
    logs.iter().filter_map(decode_transfer).collect()
}

/// Net ERC-20 balance change for one `(token, account)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erc20Delta {
    pub token: Address,
    pub account: Address,
    pub amount_in: U256,
    pub amount_out: U256,
}

impl Erc20Delta {
    /// Signed delta rendered for humans: `+N`, `-N` or `0`.
    pub fn net_str(&self) -> String {
        if self.amount_in >= self.amount_out {
            format!("+{}", self.amount_in - self.amount_out)
        } else {
            format!("-{}", self.amount_out - self.amount_in)
        }
    }

    pub fn is_positive(&self) -> bool {
        self.amount_in > self.amount_out
    }
}

/// Aggregate transfers into per-(token, account) in/out totals.
///
/// Uses `U256` saturating/checked arithmetic; overflow is impossible for real
/// token supplies but we guard anyway and skip such entries.
pub fn net_deltas(transfers: &[Transfer]) -> Vec<Erc20Delta> {
    let mut map: BTreeMap<(Address, Address), (U256, U256)> = BTreeMap::new();
    for t in transfers {
        if t.from == t.to {
            continue; // self-transfer is a no-op
        }
        // sender leg
        let e = map.entry((t.token, t.from)).or_insert((U256::ZERO, U256::ZERO));
        e.1 = e.1.saturating_add(t.value);
        // receiver leg
        let e = map.entry((t.token, t.to)).or_insert((U256::ZERO, U256::ZERO));
        e.0 = e.0.saturating_add(t.value);
        // a mint (from == 0) has no sender leg to balance; a burn likewise.
    }
    map.into_iter()
        .map(|((token, account), (amount_in, amount_out))| Erc20Delta {
            token,
            account,
            amount_in,
            amount_out,
        })
        .collect()
}

/// Compute the storage slot of `balanceOf(holder)` for a mapping living at
/// `slot_index` (Solidity `keccak256(abi.encode(holder, slot))`).
pub fn mapping_slot(holder: Address, slot_index: u64) -> U256 {
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(holder.as_slice());
    buf[32..64].copy_from_slice(&U256::from(slot_index).to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(buf).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use revm::primitives::{Bytes, LogData};

    fn transfer_log(token: Address, from: Address, to: Address, value: u64) -> Log {
        let mut t1 = [0u8; 32];
        t1[12..].copy_from_slice(from.as_slice());
        let mut t2 = [0u8; 32];
        t2[12..].copy_from_slice(to.as_slice());
        Log {
            address: token,
            data: LogData::new_unchecked(
                vec![transfer_topic0(), B256::from(t1), B256::from(t2)],
                Bytes::from(U256::from(value).to_be_bytes::<32>().to_vec()),
            ),
        }
    }

    #[test]
    fn decodes_a_real_shape_transfer() {
        let token = Address::with_last_byte(0xaa);
        let a = Address::with_last_byte(1);
        let b = Address::with_last_byte(2);
        let log = transfer_log(token, a, b, 1234);
        let t = decode_transfer(&log).expect("should decode");
        assert_eq!(t.token, token);
        assert_eq!(t.from, a);
        assert_eq!(t.to, b);
        assert_eq!(t.value, U256::from(1234));
    }

    #[test]
    fn rejects_non_transfer_logs() {
        let token = Address::with_last_byte(0xbb);
        // wrong topic0
        let log = Log {
            address: token,
            data: LogData::new_unchecked(
                vec![B256::ZERO, B256::ZERO, B256::ZERO],
                Bytes::from(vec![0u8; 32]),
            ),
        };
        assert!(decode_transfer(&log).is_none());
        // wrong arity
        let log2 = Log {
            address: token,
            data: LogData::new_unchecked(vec![transfer_topic0()], Bytes::from(vec![0u8; 32])),
        };
        assert!(decode_transfer(&log2).is_none());
    }

    #[test]
    fn nets_sender_and_receiver_legs() {
        let token = Address::with_last_byte(0xcc);
        let a = Address::with_last_byte(1);
        let b = Address::with_last_byte(2);
        let c = Address::with_last_byte(3);
        // a -> b (10), a -> c (5)
        let logs = vec![
            transfer_log(token, a, b, 10),
            transfer_log(token, a, c, 5),
        ];
        let transfers = decode_transfers(&logs);
        let deltas = net_deltas(&transfers);
        let get = |acct: Address| {
            deltas
                .iter()
                .find(|d| d.account == acct && d.token == token)
                .unwrap()
                .clone()
        };
        assert_eq!(get(a).net_str(), "-15");
        assert_eq!(get(b).net_str(), "+10");
        assert_eq!(get(c).net_str(), "+5");
    }

    #[test]
    fn mapping_slot_matches_known_vector() {
        // keccak256(abi.encode(address(0), uint256(0))) is a well-known value
        // used by the "slot 0 balances" convention (e.g. WETH/UNI pair token).
        let slot = mapping_slot(Address::ZERO, 0);
        let expected = U256::from_be_bytes(
            keccak256([0u8; 64]).0,
        );
        assert_eq!(slot, expected);
        // and it must differ for a non-zero holder
        assert_ne!(slot, mapping_slot(Address::with_last_byte(1), 0));
        assert_ne!(slot, mapping_slot(Address::ZERO, 1));
    }
}
