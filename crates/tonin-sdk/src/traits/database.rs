//! Database capability.
//!
//! Intentionally tiny — the framework's job is to hand the service a
//! connection URL + a label for telemetry. Typed access is the user's
//! DB client crate (sqlx, sea-orm), which already emits its own spans
//! when an OTel tracer provider is installed (`tonin-telemetry::init`
//! installs one). We do not reinvent a DB API on top.

use async_trait::async_trait;

/// Marker trait for a framework-managed connection pool. Reserved
/// surface — no impl ships in Phase 3. Concrete pool plumbing is a
/// service-author decision until someone genuinely asks for it.
pub trait AnyPool: Send + Sync + 'static {}

#[async_trait]
pub trait Database: Send + Sync + 'static {
    /// Connection URL — e.g. `postgres://user:pass@host:5432/db`.
    /// Per design Q4, default impls read this from the `DATABASE_URL`
    /// env var the deployment template injects.
    fn url(&self) -> &str;

    /// Optional framework-managed pool. Default `None`.
    fn pool(&self) -> Option<&dyn AnyPool> {
        None
    }

    /// Span attribute `db.system`. Implementations return one of
    /// `"postgres" | "mysql" | "sqlite" | "clickhouse"`.
    fn system(&self) -> &'static str;
}
