//! Serde types for the simulator's input file and the derived "expected"
//! ground truth captured from a real receipt.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A transaction, as written in the input JSON.
///
/// Every numeric field accepts either `"0x…"` or plain decimal so the file is
/// easy to hand-edit. `hash` is only informational (used to look up the real
/// receipt for the optional comparison).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSpec {
    pub hash: Option<String>,
    pub from: String,
    /// `null` / missing means contract creation.
    pub to: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub nonce: Option<u64>,
    #[serde(default)]
    pub gas_limit: Option<String>,
    /// Legacy / effective gas price.
    #[serde(default)]
    pub gas_price: Option<String>,
    #[serde(default)]
    pub max_fee_per_gas: Option<String>,
    #[serde(default)]
    pub max_priority_fee_per_gas: Option<String>,
    #[serde(default)]
    pub chain_id: Option<u64>,
    /// 0 legacy, 1 access-list, 2 dynamic-fee, 3 blob.
    #[serde(default)]
    pub tx_type: Option<u8>,
    #[serde(default)]
    pub blob_hashes: Option<Vec<String>>,
    #[serde(default)]
    pub max_fee_per_blob_gas: Option<String>,
}

/// Ground truth copied off the real receipt, so `run` can self-check.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Expect {
    #[serde(default)]
    pub status: Option<bool>,
    #[serde(default)]
    pub gas_used: Option<String>,
    #[serde(default)]
    pub block_number: Option<u64>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// Receipt logs as returned by the node, kept verbatim for reference.
    #[serde(default)]
    pub logs_count: Option<u64>,
}

/// The on-disk input file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimInputFile {
    /// RPC endpoint used to fetch historical state. Required to run.
    #[serde(default)]
    pub rpc_url: Option<String>,
    /// Block number whose block environment (timestamp, basefee, gas limit) we use.
    pub block: u64,
    /// Block whose *state* we load. Defaults to `block - 1`.
    #[serde(default)]
    pub state_block: Option<u64>,
    /// Hardfork, e.g. "prague"/"osaka". Defaults to auto-detection by block.
    #[serde(default)]
    pub spec: Option<String>,
    pub tx: TxSpec,
    #[serde(default)]
    pub expect: Option<Expect>,
    /// Optional map `token_address -> balance mapping slot index`, used for a
    /// direct `balanceOf` storage-slot cross-check (most tokens use slot 0, but
    /// e.g. USDC uses 9).
    #[serde(default)]
    pub erc20_balance_slots: Option<BTreeMap<String, u64>>,
    /// Optional extra addresses whose ERC20 balances we report even if they do
    /// not appear in the tx's own Transfer logs.
    #[serde(default)]
    pub watch_addresses: Option<Vec<String>>,
}
