//! Background-job entry point.
//!
//! A "job" in micro is a binary that runs to completion (queue consumer,
//! scheduled task, one-shot migration runner) rather than serving gRPC.
//! It shares the service crate, so it sees the same `State` and the same
//! `tonin::auth` setup — but it doesn't bind a port and doesn't run
//! the inbound auth layer.
//!
//! ## What `bootstrap` does
//!
//! 1. Initialize OTel (same `crate::telemetry::init` the server uses,
//!    so traces / metrics export to the same collector).
//! 2. Mint a **service-identity** `AuthCtx` via [`crate::auth::service_token`].
//!    There's no incoming request to extract auth from, so the framework
//!    mints one representing *this service* and the job propagates it on
//!    outbound RPCs.
//! 3. Build a [`crate::State`] from env (Postgres + Redis lazily, same as `main.rs`).
//!
//! ## Usage
//!
//! ```ignore
//! use tonin::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let ctx = tonin::job::bootstrap("greeter-cleanup").await?;
//!
//!     // Use ctx.state.pg() / ctx.state.redis() for queries.
//!     // Use ctx.auth.propagate(&mut req) on outbound calls so the
//!     // callee sees a service principal rather than anonymous.
//!     tracing::info!(job = "greeter-cleanup", subject = %ctx.auth.subject, "starting");
//!
//!     // ... your job logic ...
//!     Ok(())
//! }
//! ```
//!
//! ## Spawn pitfall
//!
//! [`crate::auth::CURRENT_AUTH`] is task-local and gets set by the
//! server's auth layer; **jobs don't set it** because there's no
//! inbound request. If you `tokio::spawn` from inside a job, capture
//! `ctx.auth` before the spawn and pass it explicitly.

use crate::auth::AuthCtx;
use crate::error::{Error, Result};
use crate::state::State;

/// Bootstrap output: identity + pre-wired storage. Cheap to clone.
#[derive(Clone)]
pub struct JobCtx {
    /// Service-identity auth context. Use [`AuthCtx::propagate`] on
    /// outbound requests so downstream services see this as a
    /// `PrincipalKind::Service` call.
    pub auth: AuthCtx,
    /// Postgres + Redis handles, lazily resolved from env. Same shape
    /// as the gRPC server's state.
    pub state: State,
}

/// Initialize telemetry, mint a service-identity token, and resolve
/// state. Designed to be the second line of every job binary's `main`
/// (the first being `#[tokio::main]`).
///
/// **Errors:** any of (a) the service-token minter isn't configured
/// (`TONIN_AUTH_SERVICE_TOKEN_URL` unset), (b) the auth service is
/// unreachable, (c) `DATABASE_URL` / `REDIS_URL` was set but the dep
/// is unreachable. All three are deploy-time problems; failing the
/// job at bootstrap is the right move.
pub async fn bootstrap(name: impl Into<String>) -> Result<JobCtx> {
    let name = name.into();

    // Telemetry init mirrors what `Service::new` does. We don't go
    // through Service because we don't want a port bound — but we DO
    // want the same OTel exporters wired up so the job's spans land
    // alongside the server's.
    if let Err(e) = crate::telemetry::init(&name) {
        // Non-fatal: keep the job running even if telemetry export
        // can't be set up. Same posture as the server.
        eprintln!("micro: telemetry init failed: {e}");
    }
    tracing::info!(target: "tonin::job", %name, "job bootstrapping");

    let auth = crate::auth::service_token()
        .await
        .map_err(|e| Error::Config(format!("service token mint: {e}")))?;
    let state = State::from_env().await?;

    tracing::info!(
        target: "tonin::job",
        %name,
        subject = %auth.subject,
        has_pg = state.has_pg(),
        has_redis = state.has_redis(),
        "job ready",
    );
    Ok(JobCtx { auth, state })
}
