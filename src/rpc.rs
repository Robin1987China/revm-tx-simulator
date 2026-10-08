//! Minimal blocking JSON-RPC client plus a `revm` [`DatabaseRef`] backed by a
//! remote node.
//!
//! The simulator needs to read pre-state (balances, nonces, code, storage) at a
//! *historical* block, so the RPC endpoint must serve archive requests. We keep
//! the transport deliberately simple (blocking HTTPS via `ureq`) because revm's
//! execution API is synchronous: revm will call back into this database lazily
//! while the transaction runs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use revm::database_interface::{DBErrorMarker, DatabaseRef};
use revm::primitives::{Address, B256, Bytes, StorageKey, StorageValue, U256, keccak256};
use revm::state::{AccountInfo, Bytecode};

use crate::hexutil;

/// Any failure talking to the node or decoding its answer.
#[derive(Debug)]
pub enum RpcError {
    /// Transport / HTTP level failure.
    Http(String),
    /// Node returned a JSON-RPC `error` object.
    Rpc(String),
    /// Response did not look like what we expected.
    Decode(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcError::Http(e) => write!(f, "rpc transport error: {e}"),
            RpcError::Rpc(e) => write!(f, "rpc error: {e}"),
            RpcError::Decode(e) => write!(f, "rpc decode error: {e}"),
        }
    }
}

impl std::error::Error for RpcError {}
impl DBErrorMarker for RpcError {}

impl From<RpcError> for String {
    fn from(e: RpcError) -> String {
        e.to_string()
    }
}

/// A tiny JSON-RPC 2.0 client over HTTPS.
pub struct RpcClient {
    url: String,
    agent: ureq::Agent,
    next_id: RefCell<u64>,
}

impl RpcClient {
    pub fn new(url: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(90))
            .build();
        Self { url: url.to_string(), agent, next_id: RefCell::new(1) }
    }

    #[allow(dead_code)]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Perform one JSON-RPC call and return the `result` value.
    pub fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let id = {
            let mut n = self.next_id.borrow_mut();
            let v = *n;
            *n += 1;
            v
        };
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let resp = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| RpcError::Http(e.to_string()))?;
        if resp.status() != 200 {
            return Err(RpcError::Http(format!("HTTP {}", resp.status())));
        }
        let v: serde_json::Value =
            resp.into_json().map_err(|e| RpcError::Decode(e.to_string()))?;
        if let Some(err) = v.get("error") {
            if !err.is_null() {
                return Err(RpcError::Rpc(err.to_string()));
            }
        }
        Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }

    fn as_str<'a>(v: &'a serde_json::Value, what: &str) -> Result<&'a str, RpcError> {
        v.as_str().ok_or_else(|| RpcError::Decode(format!("{what}: expected string, got {v}")))
    }

    /// `eth_getBalance`
    pub fn balance(&self, address: Address, block: u64) -> Result<U256, RpcError> {
        let r = self.call(
            "eth_getBalance",
            serde_json::json!([format!("{address:?}"), hexutil::hex_u64(block)]),
        )?;
        hexutil::u256(Self::as_str(&r, "eth_getBalance")?).map_err(RpcError::Decode)
    }

    /// `eth_getTransactionCount`
    pub fn nonce(&self, address: Address, block: u64) -> Result<u64, RpcError> {
        let r = self.call(
            "eth_getTransactionCount",
            serde_json::json!([format!("{address:?}"), hexutil::hex_u64(block)]),
        )?;
        hexutil::u64_hex(Self::as_str(&r, "eth_getTransactionCount")?).map_err(RpcError::Decode)
    }

    /// `eth_getCode`
    pub fn code(&self, address: Address, block: u64) -> Result<Bytes, RpcError> {
        let r = self.call(
            "eth_getCode",
            serde_json::json!([format!("{address:?}"), hexutil::hex_u64(block)]),
        )?;
        hexutil::bytes(Self::as_str(&r, "eth_getCode")?).map_err(RpcError::Decode)
    }

    /// `eth_getStorageAt`
    pub fn storage(
        &self,
        address: Address,
        slot: StorageKey,
        block: u64,
    ) -> Result<StorageValue, RpcError> {
        let r = self.call(
            "eth_getStorageAt",
            serde_json::json!([
                format!("{address:?}"),
                hexutil::hex_u256(slot),
                hexutil::hex_u64(block)
            ]),
        )?;
        hexutil::u256(Self::as_str(&r, "eth_getStorageAt")?).map_err(RpcError::Decode)
    }

    /// `eth_getBlockByNumber` (full = false)
    pub fn block_by_number(&self, block: u64) -> Result<serde_json::Value, RpcError> {
        self.call(
            "eth_getBlockByNumber",
            serde_json::json!([hexutil::hex_u64(block), false]),
        )
    }

    /// `eth_getTransactionByHash`
    pub fn tx_by_hash(&self, hash: &str) -> Result<serde_json::Value, RpcError> {
        self.call("eth_getTransactionByHash", serde_json::json!([hash]))
    }

    /// `eth_getTransactionReceipt`
    pub fn receipt(&self, hash: &str) -> Result<serde_json::Value, RpcError> {
        self.call("eth_getTransactionReceipt", serde_json::json!([hash]))
    }

    /// `eth_chainId`
    pub fn chain_id(&self) -> Result<u64, RpcError> {
        let r = self.call("eth_chainId", serde_json::json!([]))?;
        hexutil::u64_hex(Self::as_str(&r, "eth_chainId")?).map_err(RpcError::Decode)
    }
}

/// A `revm` database that serves account / storage reads from a remote node at
/// a fixed historical block.
pub struct RpcDb {
    client: RpcClient,
    /// Block used for every `eth_getBalance` / `eth_getStorageAt` / … call.
    state_block: u64,
    code_cache: RefCell<HashMap<B256, Bytecode>>,
}

impl RpcDb {
    pub fn new(client: RpcClient, state_block: u64) -> Self {
        Self { client, state_block, code_cache: RefCell::new(HashMap::new()) }
    }

    #[allow(dead_code)]
    pub fn state_block(&self) -> u64 {
        self.state_block
    }

    #[allow(dead_code)]
    pub fn client(&self) -> &RpcClient {
        &self.client
    }
}

impl DatabaseRef for RpcDb {
    type Error = RpcError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let nonce = self.client.nonce(address, self.state_block)?;
        let balance = self.client.balance(address, self.state_block)?;
        let raw = self.client.code(address, self.state_block)?;
        let code = Bytecode::new_raw(raw);
        let code_hash = code.hash_slow();

        if nonce == 0 && balance.is_zero() && code.is_empty() {
            return Ok(None);
        }
        if !code.is_empty() {
            self.code_cache.borrow_mut().insert(code_hash, code.clone());
        }
        Ok(Some(AccountInfo::new(balance, nonce, code_hash, code)))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if let Some(code) = self.code_cache.borrow().get(&code_hash) {
            return Ok(code.clone());
        }
        // `basic_ref` always attaches the code, so a miss here means the caller
        // asked for bytecode we never fetched. Refuse loudly rather than
        // silently simulating against empty code.
        Err(RpcError::Decode(format!(
            "bytecode {code_hash:?} not present in cache (basic_ref always loads code)"
        )))
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.client.storage(address, index, self.state_block)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        let blk = self.client.block_by_number(number)?;
        match blk.get("hash").and_then(|h| h.as_str()) {
            Some(h) => hexutil::b256(h).map_err(RpcError::Decode),
            None => Err(RpcError::Decode(format!("block {number} not found on node"))),
        }
    }
}

/// keccak256 of the empty byte string; the `code_hash` of a plain EOA.
#[allow(dead_code)]
pub fn keccak_empty() -> B256 {
    keccak256([])
}
