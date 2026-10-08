//! Building the EVM environment and running one transaction against a
//! `CacheDB`-wrapped database (real node or in-memory, for tests).

use std::collections::BTreeMap;
use std::fmt::Display;

use revm::context::result::{ExecutionResult, HaltReason};
use revm::database::CacheDB;
use revm::database_interface::DatabaseRef;
use revm::primitives::{Address, U256};
use revm::state::{AccountInfo, EvmState};
use revm::{Context, ExecuteEvm, MainBuilder, MainContext};

use crate::hexutil;

/// Everything the caller needs to explain what the transaction did.
pub struct Simulation {
    pub result: ExecutionResult<HaltReason>,
    pub state: EvmState,
    /// Pre-tx balances for every account the run touched.
    pub pre_balances: BTreeMap<Address, U256>,
    /// Pre-tx nonces for every account the run touched.
    // (kept for the CLI/report layer and tests; not every accessor is used yet)
    #[allow(dead_code)]
    pub pre_nonces: BTreeMap<Address, u64>,
    /// Pre-tx account snapshots (used for existence checks).
    #[allow(dead_code)]
    pub pre_accounts: BTreeMap<Address, Option<AccountInfo>>,
}

impl Simulation {
    pub fn is_success(&self) -> bool {
        self.result.is_success()
    }

    /// Gas actually charged for the transaction.
    pub fn gas_used(&self) -> u64 {
        match &self.result {
            ExecutionResult::Success { gas, .. } => gas.tx_gas_used(),
            ExecutionResult::Revert { gas, .. } => gas.tx_gas_used(),
            ExecutionResult::Halt { gas, .. } => gas.tx_gas_used(),
        }
    }

    /// Logs emitted by the transaction (empty on halt).
    pub fn logs(&self) -> &[revm::primitives::Log] {
        match &self.result {
            ExecutionResult::Success { logs, .. } => logs,
            ExecutionResult::Revert { logs, .. } => logs,
            ExecutionResult::Halt { logs, .. } => logs,
        }
    }

    /// Output bytes for a call, or the deployed code for a create.
    pub fn output(&self) -> &[u8] {
        match &self.result {
            ExecutionResult::Success { output, .. } => output.data(),
            ExecutionResult::Revert { output, .. } => output.as_ref(),
            ExecutionResult::Halt { .. } => &[],
        }
    }
}

/// Run `tx` against `db` with the given block environment.
///
/// `db` is a `CacheDB`, so after execution its internal cache still holds the
/// *pre-transaction* account state that revm loaded while running. That is what
/// we use to compute balance deltas.
pub fn simulate<ExtDB: DatabaseRef>(
    db: CacheDB<ExtDB>,
    block_env: revm::context::BlockEnv,
    chain_id: u64,
    spec: revm::primitives::hardfork::SpecId,
    tx: revm::context::TxEnv,
) -> Result<Simulation, String>
where
    ExtDB::Error: Display,
{
    let ctx = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = chain_id;
            cfg.set_spec_and_mainnet_gas_params(spec);
        })
        .modify_block_chained(|b| *b = block_env);

    let mut evm = ctx.build_mainnet();

    let out = evm
        .transact(tx)
        .map_err(|e| format!("evm execution error: {e:?}"))?;

    let mut pre_balances = BTreeMap::new();
    let mut pre_nonces = BTreeMap::new();
    let mut pre_accounts = BTreeMap::new();
    for (address, db_account) in evm.ctx.journaled_state.database.cache.accounts.iter() {
        pre_accounts.insert(*address, db_account.info());
        if let Some(info) = db_account.info() {
            pre_balances.insert(*address, info.balance);
            pre_nonces.insert(*address, info.nonce);
        }
    }

    Ok(Simulation {
        result: out.result,
        state: out.state,
        pre_balances,
        pre_nonces,
        pre_accounts,
    })
}

/// Build a `BlockEnv` from an `eth_getBlockByNumber` result.
pub fn block_env_from_rpc(block: &serde_json::Value) -> Result<revm::context::BlockEnv, String> {
    let s = |k: &str| -> Result<&str, String> {
        block
            .get(k)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("block field {k:?} missing"))
    };
    let number = hexutil::u64_hex(s("number")?)?;
    let timestamp = hexutil::u64_hex(s("timestamp")?)?;
    let gas_limit = hexutil::u64_hex(s("gasLimit")?)?;
    let base_fee = block
        .get("baseFeePerGas")
        .and_then(|v| v.as_str())
        .map(hexutil::u64_hex)
        .transpose()?
        .unwrap_or(0);
    let beneficiary = match block.get("miner").and_then(|v| v.as_str()) {
        Some(a) => hexutil::addr(a)?,
        None => Address::ZERO,
    };
    let prevrandao = block
        .get("mixHash")
        .and_then(|v| v.as_str())
        .map(hexutil::b256)
        .transpose()?;

    let mut env = revm::context::BlockEnv {
        number: U256::from(number),
        beneficiary,
        timestamp: U256::from(timestamp),
        gas_limit,
        basefee: base_fee,
        difficulty: U256::ZERO,
        prevrandao,
        blob_excess_gas_and_price: None,
        slot_num: 0,
    };
    if let Some(excess) = block.get("excessBlobGas").and_then(|v| v.as_str()) {
        let excess = hexutil::u64_hex(excess)?;
        env.set_blob_excess_gas_and_price(
            excess,
            revm::primitives::eip4844::BLOB_BASE_FEE_UPDATE_FRACTION_PRAGUE,
        );
    }
    Ok(env)
}

/// Map a spec name to a `SpecId`.
pub fn spec_from_name(name: &str) -> Result<revm::primitives::hardfork::SpecId, String> {
    use revm::primitives::hardfork::SpecId;
    let n = name.trim().to_ascii_lowercase();
    Ok(match n.as_str() {
        "frontier" => SpecId::FRONTIER,
        "homestead" => SpecId::HOMESTEAD,
        "byzantium" => SpecId::BYZANTIUM,
        "istanbul" => SpecId::ISTANBUL,
        "berlin" => SpecId::BERLIN,
        "london" => SpecId::LONDON,
        "merge" | "paris" => SpecId::MERGE,
        "shanghai" => SpecId::SHANGHAI,
        "cancun" => SpecId::CANCUN,
        "prague" | "pectra" => SpecId::PRAGUE,
        "osaka" | "fusaka" => SpecId::OSAKA,
        "amsterdam" => SpecId::AMSTERDAM,
        other => return Err(format!("unknown spec {other:?}")),
    })
}

/// Best-effort mainnet hardfork detection by block number.
///
/// Sources: revm's own `SpecId` doc comments (activation blocks). Anything at or
/// beyond the Osaka activation is reported as Osaka; the caller can always
/// override with `spec`.
pub fn spec_for_block(block: u64) -> revm::primitives::hardfork::SpecId {
    use revm::primitives::hardfork::SpecId;
    const SHANGHAI: u64 = 17_034_870;
    const CANCUN: u64 = 19_426_587;
    const PRAGUE: u64 = 22_431_084;
    const OSAKA: u64 = 23_935_694;
    if block >= OSAKA {
        SpecId::OSAKA
    } else if block >= PRAGUE {
        SpecId::PRAGUE
    } else if block >= CANCUN {
        SpecId::CANCUN
    } else if block >= SHANGHAI {
        SpecId::SHANGHAI
    } else {
        SpecId::MERGE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_lookup_roundtrip() {
        assert_eq!(spec_from_name("prague").unwrap(), revm::primitives::hardfork::SpecId::PRAGUE);
        assert_eq!(spec_from_name("OSAKA").unwrap(), revm::primitives::hardfork::SpecId::OSAKA);
        assert!(spec_from_name("nope").is_err());
    }

    #[test]
    fn spec_for_block_is_monotonic() {
        use revm::primitives::hardfork::SpecId;
        assert_eq!(spec_for_block(22_431_084), SpecId::PRAGUE);
        assert_eq!(spec_for_block(23_935_694), SpecId::OSAKA);
        assert_eq!(spec_for_block(25_000_000), SpecId::OSAKA);
        assert_eq!(spec_for_block(19_426_587), SpecId::CANCUN);
    }

    #[test]
    fn block_env_parses_real_shape() {
        let blk = serde_json::json!({
            "number": "0x18ef024",
            "timestamp": "0x68e0a000",
            "gasLimit": "0x1c9c380",
            "baseFeePerGas": "0x3b9aca00",
            "miner": "0x0000000000000000000000000000000000000000",
            "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "excessBlobGas": "0x0"
        });
        let env = block_env_from_rpc(&blk).unwrap();
        assert_eq!(env.number, U256::from(0x18ef024u64));
        assert_eq!(env.basefee, 1_000_000_000);
        assert_eq!(env.gas_limit, 30_000_000);
    }
}
