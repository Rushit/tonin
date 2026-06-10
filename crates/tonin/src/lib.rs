//! tonin CLI library surface.
//!
//! This crate is the `tonin` CLI binary. It has no runtime API for service
//! authors — use [`tonin-sdk`](https://docs.rs/tonin-sdk) for the framework.
//!
//! The `codegen` and `commands` modules are exposed so `src/bin/tonin.rs`
//! can reach them as `tonin::codegen` / `tonin::commands`.

#[cfg(feature = "cli")]
pub mod codegen;
#[cfg(feature = "cli")]
pub mod commands;
