//! Library surface of `revm-txsim`.
//!
//! The binary in `src/main.rs` is a thin CLI on top of these modules, and the
//! integration tests in `tests/` drive the same functions directly.

pub mod diff;
pub mod erc20;
pub mod hexutil;
pub mod model;
pub mod rpc;
pub mod sim;
