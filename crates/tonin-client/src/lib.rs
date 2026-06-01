//! Tiny peer-service client primitives. No server-framework deps.
//!
//! # When to use
//!
//! Service B calls Service A — pull this in, **not** [`tonin-core`],
//! which drags the tonic server stack, OTLP telemetry SDK, MCP sidecar
//! runtime, JWT validation and JWKS fetching along with it. This crate
//! holds only the contract pieces a caller actually needs: auth context,
//! retry/breaker config, and W3C trace propagation.
//!
//! # Example
//!
//! Propagate an inbound `AuthCtx` + `traceparent` onto an outbound tonic
//! request:
//!
//! ```rust,no_run
//! use tonin_client::{AuthCtx, propagate};
//! use tonic::Request;
//!
//! fn forward(inbound: &Request<()>) -> Request<()> {
//!     let caller = AuthCtx::from(inbound);
//!     let mut outbound = Request::new(());
//!     caller.propagate(&mut outbound);
//!     propagate::inject_traceparent(
//!         &mut outbound,
//!         "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
//!     );
//!     outbound
//! }
//! ```
//!
//! # Modules
//!
//! - [`auth`] — `AuthCtx`, `RawToken`, `PrincipalKind`, `AuthError`.
//! - [`retry`] — `RetryPolicy`, `Backoff`, `RetryableCodes`.
//! - [`breaker`] — `CircuitBreaker` config with three-state presets.
//! - [`propagate`] — W3C `traceparent` / `tracestate` injection.
//!
//! # Sample app
//!
//! <https://github.com/Rushit/tonin/tree/main/examples/greeter> —
//! Service B in the demo depends on this crate to call Service A.
//!
//! # Sibling crates
//!
//! - <https://docs.rs/tonin> — umbrella crate for service authors.
//! - <https://docs.rs/tonin-core> — server runtime (use on the
//!   service being called, not on the caller).
//!
//! [`tonin-core`]: https://docs.rs/tonin-core

pub mod auth;
pub mod breaker;
pub mod propagate;
pub mod retry;

pub use auth::{AuthCtx, AuthError, PrincipalKind, RawToken};
