//! `revm-txsim` — replay a real (or hand-written) transaction locally against
//! historical chain state and report exactly what changed.
//!
//! Subcommands:
//!   fetch <txhash>   pull a real tx + receipt from a node and write an input file
//!   run              simulate an input file and print the diff

use revm_txsim::{diff, erc20, hexutil, model, rpc, sim};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use revm::database::CacheDB;
use revm::primitives::{Address, TxKind, U256};
use revm::Context;

use model::{Expect, SimInputFile, TxSpec};
use rpc::{RpcClient, RpcDb};
use sim::Simulation;

const DEFAULT_RPC: &str = "https://eth.drpc.org";

fn usage() -> String {
    r#"revm-txsim — local transaction simulator (revm)

USAGE:
  revm-txsim fetch <TX_HASH> [--rpc URL] [--out FILE]
      Fetch a real transaction + receipt and write a ready-to-run input file.

  revm-txsim run --input FILE [--rpc URL] [--out FILE] [--spec NAME] [--no-nonce-check]
      Simulate the transaction in the input file and print / save the diff.

  revm-txsim slot --holder ADDR [--max N]
      Print keccak256(abi.encode(holder, i)) for i in 0..N, i.e. the candidate
      storage slots of a Solidity `mapping(address => uint256)`. Use it to work
      out which slot index a token uses.

OPTIONS:
  --rpc URL         JSON-RPC endpoint (archive node needed for old blocks).
                    Defaults to the file's rpc_url, then $REVM_TXSIM_RPC, then
                    https://eth.drpc.org
  --out FILE        Write JSON output to FILE as well as stdout.
  --spec NAME       Hardfork override (prague, osaka, cancun, ...).
  --no-nonce-check  Skip transaction-nonce validation (useful when the state
                    snapshot block is only approximate).
"#
    .to_string()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    let result = match args[0].as_str() {
        "fetch" => cmd_fetch(&args[1..]),
        "run" => cmd_run(&args[1..]),
        "slot" => cmd_slot(&args[1..]),
        other => {
            eprintln!("unknown subcommand {other:?}\n\n{}", usage());
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Minimal `--flag value` options parser.
fn parse_opts(args: &[String], value_flags: &[&str], bool_flags: &[&str])
    -> Result<(Vec<String>, BTreeMap<String, String>, Vec<String>), String>
{
    let mut positional = Vec::new();
    let mut values = BTreeMap::new();
    let mut bools = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if value_flags.contains(&a.as_str()) {
            let v = args.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
            values.insert(a.clone(), v.clone());
            i += 2;
        } else if bool_flags.contains(&a.as_str()) {
            bools.push(a.clone());
            i += 1;
        } else {
            positional.push(a.clone());
            i += 1;
        }
    }
    Ok((positional, values, bools))
}

fn resolve_rpc(file_rpc: Option<&str>, cli: Option<&str>) -> String {
    cli.map(|s| s.to_string())
        .or_else(|| file_rpc.map(|s| s.to_string()))
        .or_else(|| std::env::var("REVM_TXSIM_RPC").ok())
        .unwrap_or_else(|| DEFAULT_RPC.to_string())
}

fn write_out(out: Option<&String>, text: &str) -> Result<(), String> {
    if let Some(path) = out {
        std::fs::write(PathBuf::from(path), text).map_err(|e| format!("cannot write {path}: {e}"))?;
        eprintln!("wrote {path}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// slot
// ---------------------------------------------------------------------------

/// Print candidate `mapping(address => uint256)` slots for a holder.
fn cmd_slot(args: &[String]) -> Result<(), String> {
    let (_pos, opts, _) = parse_opts(args, &["--holder", "--max"], &[])?;
    let holder = opts.get("--holder").ok_or("slot requires --holder ADDR")?;
    let holder = hexutil::addr(holder)?;
    let max: u64 = opts
        .get("--max")
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|e| format!("bad --max: {e}"))?
        .unwrap_or(16);
    for i in 0..max {
        let slot = erc20::mapping_slot(holder, i);
        println!("{i:>3}  {}", hexutil::hex_u256(slot));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

fn cmd_fetch(args: &[String]) -> Result<(), String> {
    let (pos, opts, _) = parse_opts(args, &["--rpc", "--out"], &[])?;
    let hash = pos.first().ok_or("fetch requires a transaction hash")?;
    let rpc_url = resolve_rpc(None, opts.get("--rpc").map(|s| s.as_str()));
    let out = opts.get("--out");

    let client = RpcClient::new(&rpc_url);
    eprintln!("fetching {hash} from {rpc_url} ...");

    let tx = client.tx_by_hash(hash)?;
    if tx.is_null() {
        return Err(format!("transaction {hash} not found on {rpc_url}"));
    }
    let receipt = client.receipt(hash)?;
    if receipt.is_null() {
        return Err(format!("receipt for {hash} not found (may be pending)"));
    }
    let block_number = hexutil::u64_hex(get_str(&receipt, "blockNumber")?)?;

    let tx_spec = TxSpec {
        hash: Some(hash.clone()),
        from: get_str(&tx, "from")?.to_string(),
        to: tx.get("to").and_then(|v| v.as_str()).map(|s| s.to_string()),
        value: tx.get("value").and_then(|v| v.as_str()).map(|s| s.to_string()),
        data: tx.get("input").and_then(|v| v.as_str()).map(|s| s.to_string()),
        nonce: tx.get("nonce").and_then(|v| v.as_str()).map(hexutil::u64_hex).transpose()?,
        gas_limit: tx.get("gas").and_then(|v| v.as_str()).map(|s| s.to_string()),
        gas_price: tx.get("gasPrice").and_then(|v| v.as_str()).map(|s| s.to_string()),
        max_fee_per_gas: tx.get("maxFeePerGas").and_then(|v| v.as_str()).map(|s| s.to_string()),
        max_priority_fee_per_gas: tx
            .get("maxPriorityFeePerGas")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        chain_id: tx
            .get("chainId")
            .and_then(|v| v.as_str())
            .map(hexutil::u64_hex)
            .transpose()?,
        tx_type: tx.get("type").and_then(|v| v.as_str()).map(hexutil::u64_hex).transpose()?.map(|v| v as u8),
        blob_hashes: tx
            .get("blobVersionedHashes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()),
        max_fee_per_blob_gas: tx
            .get("maxFeePerBlobGas")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    };

    let logs_count = receipt.get("logs").and_then(|v| v.as_array()).map(|a| a.len() as u64);

    let expect = Expect {
        status: receipt.get("status").and_then(|v| v.as_str()).map(|s| s == "0x1"),
        gas_used: receipt.get("gasUsed").and_then(|v| v.as_str()).map(|s| s.to_string()),
        block_number: Some(block_number),
        from: Some(get_str(&tx, "from")?.to_string()),
        to: tx.get("to").and_then(|v| v.as_str()).map(|s| s.to_string()),
        logs_count,
    };

    let input = SimInputFile {
        rpc_url: Some(rpc_url.clone()),
        block: block_number,
        state_block: None,
        spec: None,
        tx: tx_spec,
        expect: Some(expect),
        erc20_balance_slots: None,
        watch_addresses: None,
    };

    let text = serde_json::to_string_pretty(&input).map_err(|e| e.to_string())?;
    println!("{text}");
    let default_out = format!("out/input-{}.json", &hash[2..10.min(hash.len())]);
    write_out(out.or(Some(&default_out)), &text)?;
    Ok(())
}

fn get_str<'a>(v: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("field {key:?} missing in RPC response"))
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

fn cmd_run(args: &[String]) -> Result<(), String> {
    let (_pos, opts, bools) = parse_opts(
        args,
        &["--input", "--rpc", "--out", "--spec"],
        &["--no-nonce-check"],
    )?;
    let input_path = opts.get("--input").ok_or("run requires --input FILE")?;
    let raw = std::fs::read_to_string(input_path)
        .map_err(|e| format!("cannot read {input_path}: {e}"))?;
    let input: SimInputFile =
        serde_json::from_str(&raw).map_err(|e| format!("cannot parse {input_path}: {e}"))?;

    let rpc_url = resolve_rpc(input.rpc_url.as_deref(), opts.get("--rpc").map(|s| s.as_str()));
    let block = input.block;
    let state_block = input.state_block.unwrap_or(block.saturating_sub(1));
    let spec = match opts.get("--spec").or(input.spec.as_ref()) {
        Some(name) => sim::spec_from_name(name)?,
        None => sim::spec_for_block(block),
    };
    let no_nonce_check = bools.iter().any(|b| b == "--no-nonce-check");

    let client = RpcClient::new(&rpc_url);
    eprintln!(
        "simulating tx {} in block {block} (state @ {state_block}, spec {:?}) via {rpc_url}",
        input.tx.hash.as_deref().unwrap_or("<no hash>"),
        spec
    );

    let block_json = client.block_by_number(block)?;
    if block_json.is_null() {
        return Err(format!("block {block} not available on {rpc_url}"));
    }
    let block_env = sim::block_env_from_rpc(&block_json)?;

    let chain_id = input.tx.chain_id.unwrap_or_else(|| client.chain_id().unwrap_or(1));

    let tx_env = build_tx_env(&input.tx, chain_id)?;

    let rpc_db = RpcDb::new(client, state_block);
    let db = CacheDB::new(rpc_db);

    let simulation = run_with_optional_nonce_check(db, block_env, chain_id, spec, tx_env, no_nonce_check)?;

    let report = build_report(&input, &rpc_url, block, state_block, spec, &simulation)?;
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    println!("{text}");
    write_out(opts.get("--out"), &text)?;
    Ok(())
}

/// revm's `CfgEnv::disable_nonce_check` is the supported escape hatch, but it
/// lives on the context, so we branch here rather than threading a flag through
/// `simulate`.
fn run_with_optional_nonce_check<ExtDB: revm::database_interface::DatabaseRef>(
    db: CacheDB<ExtDB>,
    block_env: revm::context::BlockEnv,
    chain_id: u64,
    spec: revm::primitives::hardfork::SpecId,
    tx: revm::context::TxEnv,
    no_nonce_check: bool,
) -> Result<Simulation, String>
where
    ExtDB::Error: std::fmt::Display,
{
    if !no_nonce_check {
        return sim::simulate(db, block_env, chain_id, spec, tx);
    }
    use revm::{ExecuteEvm, MainBuilder, MainContext};
    let ctx = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = chain_id;
            cfg.set_spec_and_mainnet_gas_params(spec);
            cfg.disable_nonce_check = true;
        })
        .modify_block_chained(|b| *b = block_env);
    let mut evm = ctx.build_mainnet();
    let out = evm.transact(tx).map_err(|e| format!("evm execution error: {e:?}"))?;
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

/// Turn the input `TxSpec` into a revm `TxEnv`.
fn build_tx_env(spec: &TxSpec, chain_id: u64) -> Result<revm::context::TxEnv, String> {
    let caller = hexutil::addr(&spec.from)?;
    let kind = match &spec.to {
        Some(to) => TxKind::Call(hexutil::addr(to)?),
        None => TxKind::Create,
    };
    let value = match &spec.value {
        Some(v) => hexutil::u256(v)?,
        None => U256::ZERO,
    };
    let data = match &spec.data {
        Some(d) => hexutil::bytes(d)?,
        None => revm::primitives::Bytes::new(),
    };
    let gas_limit = match &spec.gas_limit {
        Some(g) => hexutil::u64_flexible(g)?,
        None => 30_000_000,
    };
    let tx_type = spec.tx_type;

    let mut builder = revm::context::TxEnv::builder()
        .caller(caller)
        .kind(kind)
        .value(value)
        .data(data)
        .gas_limit(gas_limit)
        .nonce(spec.nonce.unwrap_or(0))
        .chain_id(Some(chain_id));

    match tx_type {
        // dynamic-fee / blob: gas_price == max_fee_per_gas, plus the tip
        Some(2) | Some(3) => {
            let max_fee = spec
                .max_fee_per_gas
                .as_ref()
                .map(|s| hexutil::u256(s))
                .transpose()?
                .unwrap_or(U256::ZERO);
            builder = builder
                .gas_price(max_fee.try_into().map_err(|_| "max_fee_per_gas too large")?)
                .gas_priority_fee(Some(
                    spec.max_priority_fee_per_gas
                        .as_ref()
                        .map(|s| hexutil::u256(s))
                        .transpose()?
                        .unwrap_or(U256::ZERO)
                        .try_into()
                        .map_err(|_| "max_priority_fee_per_gas too large")?,
                ));
        }
        _ => {
            let gas_price = spec
                .gas_price
                .as_ref()
                .map(|s| hexutil::u256(s))
                .transpose()?
                .unwrap_or(U256::ZERO);
            builder = builder.gas_price(
                gas_price.try_into().map_err(|_| "gas_price too large")?,
            );
        }
    }

    if let Some(t) = tx_type {
        builder = builder.tx_type(Some(t));
    }
    if let Some(hashes) = &spec.blob_hashes {
        let mut hs = Vec::new();
        for h in hashes {
            hs.push(hexutil::b256(h)?);
        }
        builder = builder.blob_hashes(hs);
    }
    if let Some(fee) = &spec.max_fee_per_blob_gas {
        let v = hexutil::u256(fee)?;
        builder = builder
            .max_fee_per_blob_gas(v.try_into().map_err(|_| "max_fee_per_blob_gas too large")?);
    }

    builder.build().map_err(|e| format!("invalid tx env: {e:?}"))
}

// ---------------------------------------------------------------------------
// report
// ---------------------------------------------------------------------------

fn build_report(
    input: &SimInputFile,
    rpc_url: &str,
    block: u64,
    state_block: u64,
    spec: revm::primitives::hardfork::SpecId,
    simulation: &Simulation,
) -> Result<serde_json::Value, String> {
    let success = simulation.is_success();
    let gas_used = simulation.gas_used();

    let balances = diff::diff_balances(&simulation.pre_balances, &simulation.state);
    let nonces = diff::diff_nonces(&simulation.pre_nonces, &simulation.state);
    let storage = diff::diff_storage(&simulation.state);

    let transfers = erc20::decode_transfers(simulation.logs());
    let deltas = erc20::net_deltas(&transfers);
    let mut erc20_rows: Vec<serde_json::Value> = Vec::new();
    for d in &deltas {
        erc20_rows.push(serde_json::json!({
            "token": format!("{:?}", d.token),
            "account": format!("{:?}", d.account),
            "in": d.amount_in.to_string(),
            "out": d.amount_out.to_string(),
            "net": d.net_str(),
        }));
    }

    // Optional direct storage-slot cross-check for explicitly configured tokens.
    let mut slot_checks: Vec<serde_json::Value> = Vec::new();
    if let Some(slots) = &input.erc20_balance_slots {
        for (token, idx) in slots {
            let token_addr = hexutil::addr(token)?;
            let mut holders: Vec<Address> = Vec::new();
            holders.push(hexutil::addr(&input.tx.from)?);
            if let Some(to) = &input.tx.to {
                holders.push(hexutil::addr(to)?);
            }
            for w in input.watch_addresses.iter().flatten() {
                holders.push(hexutil::addr(w)?);
            }
            holders.sort();
            holders.dedup();
            holders.retain(|h| *h != token_addr);
            for holder in holders {
                let slot = erc20::mapping_slot(holder, *idx);
                let after = simulation
                    .state
                    .get(&token_addr)
                    .and_then(|a| a.storage.get(&slot))
                    .map(|s| s.present_value)
                    .unwrap_or(U256::ZERO);
                // The pre-value is authoritative only if revm loaded the slot
                // (then the diff carries `original_value`); otherwise 0.
                let before = before_from_original(simulation, token_addr, slot);
                slot_checks.push(serde_json::json!({
                    "token": format!("{token_addr:?}"),
                    "holder": format!("{holder:?}"),
                    "balance_slot_index": idx,
                    "slot": hexutil::hex_u256(slot),
                    "before": hexutil::hex_u256(before),
                    "after": hexutil::hex_u256(after),
                }));
            }
        }
    }

    let expect_check = input.expect.as_ref().map(|e| {
        let gas_ok = e
            .gas_used
            .as_ref()
            .and_then(|g| hexutil::u64_hex(g).ok())
            .map(|real| real == gas_used);
        serde_json::json!({
            "status_expected": e.status,
            "status_actual": success,
            "status_match": e.status.map(|s| s == success),
            "gas_used_expected_dec": e.gas_used.as_ref().and_then(|g| hexutil::u64_hex(g).ok()),
            "gas_used_simulated": gas_used,
            "gas_used_match": gas_ok,
            "receipt_logs_count": e.logs_count,
        })
    });

    let logs_json: Vec<serde_json::Value> = simulation
        .logs()
        .iter()
        .map(|l| {
            serde_json::json!({
                "address": format!("{:?}", l.address),
                "topics": l.data.topics().iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
                "data": format!("0x{}", revm::primitives::hex::encode(l.data.data.as_ref())),
            })
        })
        .collect();

    let balance_rows: Vec<serde_json::Value> =
        balances.iter().map(diff::balance_change_json).collect();
    let storage_rows: Vec<serde_json::Value> =
        storage.iter().map(diff::storage_change_json).collect();
    let nonce_rows: Vec<serde_json::Value> = nonces
        .iter()
        .map(|n| {
            serde_json::json!({
                "address": format!("{:?}", n.address),
                "before": n.before,
                "after": n.after,
            })
        })
        .collect();

    // Accounts whose code appeared during the tx (contract creation).
    let created: Vec<String> = simulation
        .state
        .iter()
        .filter(|(addr, acc)| {
            let had_code = simulation
                .pre_accounts
                .get(*addr)
                .and_then(|o| o.as_ref())
                .map(|i| i.code.as_ref().map(|c| !c.is_empty()).unwrap_or(false))
                .unwrap_or(false);
            let has_code = acc.info.code.as_ref().map(|c| !c.is_empty()).unwrap_or(false);
            !had_code && has_code
        })
        .map(|(addr, _)| format!("{addr:?}"))
        .collect();

    Ok(serde_json::json!({
        "input_file": {
            "tx_hash": input.tx.hash,
            "block": block,
            "state_block": state_block,
            "spec": format!("{spec:?}"),
            "rpc_url": rpc_url,
        },
        "execution": {
            "success": success,
            "gas_used": gas_used,
            "output_hex": format!("0x{}", revm::primitives::hex::encode(simulation.output())),
            "logs_count": simulation.logs().len(),
        },
        "expect_check": expect_check,
        "eth_balance_changes": balance_rows,
        "nonce_changes": nonce_rows,
        "accounts_created": created,
        "storage_diff": storage_rows,
        "erc20_transfers": transfers
            .iter()
            .map(|t| serde_json::json!({
                "token": format!("{:?}", t.token),
                "from": format!("{:?}", t.from),
                "to": format!("{:?}", t.to),
                "value": t.value.to_string(),
            }))
            .collect::<Vec<_>>(),
        "erc20_net_balance_changes": erc20_rows,
        "erc20_slot_checks": slot_checks,
        "logs": logs_json,
    }))
}

/// Pre-value of a `balanceOf` slot: we only have it if the slot was loaded and
/// changed (then `original_value` is authoritative). Otherwise fall back to 0.
fn before_from_original(simulation: &Simulation, token: Address, slot: U256) -> U256 {
    simulation
        .state
        .get(&token)
        .and_then(|a| a.storage.get(&slot))
        .map(|s| s.original_value)
        .unwrap_or(U256::ZERO)
}
