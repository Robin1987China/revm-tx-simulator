//! Small hex / integer parsing helpers shared by the CLI and the RPC layer.
//!
//! JSON-RPC returns "0x"-prefixed hex for everything, and hand-written input
//! files may use either hex or decimal for numbers, so keep the parsing rules
//! in one place and unit-test them.

use revm::primitives::{hex, Address, B256, Bytes, U256};
use std::str::FromStr;

/// Parse an address from "0x…" (40 hex chars).
pub fn addr(s: &str) -> Result<Address, String> {
    let s = s.trim();
    Address::from_str(s).map_err(|e| format!("bad address {s:?}: {e}"))
}

/// Parse a 32-byte hash from "0x…".
pub fn b256(s: &str) -> Result<B256, String> {
    let s = s.trim();
    B256::from_str(s).map_err(|e| format!("bad 32-byte hash {s:?}: {e}"))
}

/// Parse a hex byte string ("0x" prefix optional, empty body allowed).
pub fn bytes(s: &str) -> Result<Bytes, String> {
    let s = s.trim();
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    if body.is_empty() {
        return Ok(Bytes::new());
    }
    hex::decode(body)
        .map(Bytes::from)
        .map_err(|e| format!("bad hex bytes: {e}"))
}

/// Parse a 256-bit integer given as "0x…" or plain decimal.
pub fn u256(s: &str) -> Result<U256, String> {
    let s = s.trim();
    if let Some(body) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if body.is_empty() {
            return Ok(U256::ZERO);
        }
        U256::from_str_radix(body, 16).map_err(|e| format!("bad hex u256 {s:?}: {e}"))
    } else {
        U256::from_str_radix(s, 10).map_err(|e| format!("bad u256 {s:?}: {e}"))
    }
}

/// Parse a JSON-RPC hex quantity such as "0x1a" into u64.
pub fn u64_hex(s: &str) -> Result<u64, String> {
    let body = s.trim().strip_prefix("0x").or_else(|| s.trim().strip_prefix("0X")).unwrap_or(s.trim());
    if body.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(body, 16).map_err(|e| format!("bad hex u64 {s:?}: {e}"))
}

/// Parse a decimal-or-hex integer into u64 (input files are lenient).
pub fn u64_flexible(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(body) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64_hex(body)
    } else {
        s.parse::<u64>().map_err(|e| format!("bad u64 {s:?}: {e}"))
    }
}

/// `0x`-prefixed lowercase hex for a 32-byte quantity.
pub fn hex_u256(v: U256) -> String {
    format!("{v:#064x}")
}

/// `0x`-prefixed lowercase hex for u64.
pub fn hex_u64(v: u64) -> String {
    format!("0x{v:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_roundtrip() {
        let a = addr("0x0000000000000000000000000000000000000001").unwrap();
        assert_eq!(format!("{a:?}").to_lowercase().contains("0000000000000000000000000000000000000001"), true);
    }

    #[test]
    fn bad_address_is_rejected() {
        assert!(addr("0x1234").is_err());
        assert!(addr("not-an-address").is_err());
    }

    #[test]
    fn u256_accepts_hex_and_decimal() {
        assert_eq!(u256("0x10").unwrap(), U256::from(16));
        assert_eq!(u256("16").unwrap(), U256::from(16));
        assert_eq!(u256("0x").unwrap(), U256::ZERO);
        assert!(u256("0xzz").is_err());
    }

    #[test]
    fn bytes_handles_empty_and_prefix() {
        assert_eq!(bytes("0x").unwrap().len(), 0);
        assert_eq!(bytes("0xdeadbeef").unwrap().len(), 4);
        assert_eq!(bytes("deadbeef").unwrap().len(), 4);
        assert!(bytes("0xdeg0").is_err());
    }

    #[test]
    fn u64_helpers() {
        assert_eq!(u64_hex("0x1a").unwrap(), 26);
        assert_eq!(u64_flexible("26").unwrap(), 26);
        assert_eq!(u64_flexible("0x1a").unwrap(), 26);
    }
}
