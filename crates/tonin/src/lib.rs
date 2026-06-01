//! Kubernetes-native Rust microservice framework with telemetry, auth, and MCP
//! built in.
//!
//! # Example
//!
//! ```no_run
//! use tonin::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> tonin::Result<()> {
//!     // Service::new installs OTLP tracing + a log subscriber.
//!     // Add `.handler(GreeterServer::new(MyImpl))` once you have a
//!     // tonic-generated service to bind.
//!     Service::new("greeter").run().await
//! }
//! ```
//!
//! # Re-exports
//!
//! - [`Service`] — builder that wires telemetry, auth, MCP, and tonic.
//! - [`Error`] — framework error type (`Result<T>` alias also available).
//! - [`State`] — typed shared state container for handlers.
//! - [`prelude`] — glob-import the essentials (`Service`, `Error`, `State`,
//!   `AuthCtx`, `tonic::{Request, Response, Status}`).
//! - [`mcp_expose`] — proc-macro attribute; auto-derive an MCP adapter
//!   from a gRPC `impl` block (one tool per method).
//! - [`auth`] — JWT validation, `AuthCtx`, `AuthLayer`.
//! - [`telemetry`] — zero-config OTLP tracing + W3C TraceContext.
//! - [`transport`] — tonic/gRPC wiring (mTLS delegated to the mesh).
//! - [`discovery`] — k8s DNS resolution for peer services.
//! - [`mcp`] — MCP sidecar runtime and tool registry.
//!
//! `Result<T>` is reachable as `tonin::Result` but is intentionally **not**
//! in [`prelude`] — it would shadow `std::result::Result` and break
//! macro-generated code emitting unqualified `Result<T, E>`.
//!
//! # Sample app
//!
//! Full end-to-end gRPC + MCP service:
//! <https://github.com/Rushit/tonin/tree/main/examples/greeter>
//!
//! # Sibling crates
//!
//! - [`tonin-core`](https://docs.rs/tonin-core) — runtime, traits, and
//!   submodules (`transport`, `discovery`, `mcp`, `telemetry`, `auth`, `state`,
//!   `job`). Depend on this directly for lighter builds.
//! - [`tonin-client`](https://docs.rs/tonin-client) — peer-service client
//!   primitives (`AuthCtx`, retry, breaker, header propagation) without the
//!   server framework.
//! - [`tonin-mcp-macros`](https://docs.rs/tonin-mcp-macros) — the
//!   `#[mcp_expose]` proc-macro crate.
//! - [`tonin-build`](https://docs.rs/tonin-build) — `build.rs` helper
//!   wrapping `tonic-build` with tonin conventions.
//!
//! # CLI
//!
//! This crate also ships the `tonin` binary (scaffold services, run codegen,
//! render k8s manifests). Default features include `cli` so `cargo install
//! tonin` installs the binary. Library consumers who don't want the CLI's
//! dep tree should pass `default-features = false`.

pub use tonin_core as core;
pub use tonin_core::{discovery, mcp, telemetry, transport};

// CLI internals — pub so the in-crate `tonin` binary at `src/bin/tonin.rs`
// can reach them as `tonin::codegen` / `tonin::commands`. Library
// consumers who set `default-features = false` skip these entirely.
#[cfg(feature = "cli")]
pub mod codegen;
#[cfg(feature = "cli")]
pub mod commands;

// Re-export commonly used top-level types so users can write
// `use tonin::Service;` or `use tonin::auth::JwtValidator;` directly.
pub use tonin_core::{Error, Result, Service, State, auth, job, state};

// Proc-macro: places on the user's `impl Greeter for GreeterImpl` block
// to auto-derive an MCP adapter (one tool per gRPC method).
pub use tonin_core::mcp::mcp_expose;

pub mod prelude {
    pub use tonin_core::prelude::*;
}

/// Placeholder for `#[tonin::main]`: an attribute macro that wires the
/// tracing subscriber + tokio runtime. Today this is just a re-export of
/// `#[tokio::main]`; the dedicated proc-macro will land in a follow-up.
pub use tokio::main;
