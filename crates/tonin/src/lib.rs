//! tonin CLI library surface.
//!
//! This crate is the `tonin` CLI binary. It has no runtime API for service
//! authors — use [`tonin-sdk`](https://docs.rs/tonin-sdk) for the framework.
//!
//! The `commands` module is exposed so `src/bin/tonin.rs` can reach it as
//! `tonin::commands`.

#[cfg(feature = "cli")]
pub mod commands;
