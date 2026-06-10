//! Pre-wired DB + cache connections.
//!
//! [`State`] holds the connection handles a service needs. It's
//! constructed at boot via [`State::from_env`], threaded through to
//! handlers as `Arc<State>` (or via a builder field), and used by app
//! code with sqlx / redis directly. No abstraction layer — the
//! framework owns the *connection*, you own the *queries*.
//!
//! ## Activation
//!
//! All fields are optional. DB + cache are always-emitted (env-driven);
//! object storage is scaffold-time opt-in via `tonin service new
//! --with-storage <kind>`:
//!
//! | Field          | Source                                          |
//! |----------------|-------------------------------------------------|
//! | `pg`           | `DATABASE_URL`                                  |
//! | `redis`        | `REDIS_URL`                                     |
//! | `storage`      | scaffolded `--with-storage` + `STORAGE_*` env   |
//!
//! Absent env → field stays `None`, no connection attempt. So a service
//! that doesn't use a given backend still compiles and runs — calling
//! `state.pg()` / `state.storage()` returns `Err(Error::Config("…not
//! set"))` if reached.
//!
//! ## Extensibility — `StorageProvider` trait
//!
//! Object storage hides behind a [`StorageProvider`] trait so users
//! can swap the default opendal-based implementation for anything
//! else (a custom client, an in-memory mock, a different SDK) without
//! touching the framework. The scaffold's `--with-storage` flag emits
//! an `OpendalStorage` impl into `state.rs`, but any
//! `Arc<dyn StorageProvider>` works in its place.
//!
//! ## Why concrete types, not traits
//!
//! The `Database` / `Cache` traits in [`crate::traits`] exist for
//! capability discovery (telemetry attributes, swappable impls down
//! the road). For the actual query path, services should use sqlx /
//! redis directly — those libraries already produce OTel spans when a
//! tracer provider is installed, and wrapping them in a thin trait
//! layer mostly costs ergonomics.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{Error, Result};

/// Pluggable object-storage backend.
///
/// The default impl emitted by `--with-storage` wraps an
/// `opendal::Operator`, but anything matching this trait works. The
/// framework only calls [`StorageProvider::probe`] at boot — every
/// other call goes through whatever concrete API the impl exposes
/// (so users get full opendal surface without going through this
/// trait if they're using the default).
///
/// To wire a custom provider: construct it yourself, then
/// [`State::with_storage`] (no need to set env vars).
#[async_trait]
pub trait StorageProvider: Send + Sync + 'static {
    /// Cheap connectivity check. The scaffold's default impl calls
    /// `Operator::list("/").limit(1).await` and ignores the contents
    /// — we just want to know we can talk to the bucket. Errors
    /// here should cause the service to fail to start.
    async fn probe(&self) -> Result<()>;

    /// Tag used in span attributes (`storage.system`). Conventions:
    /// `"s3"`, `"gcs"`, `"azure"`, `"local"`, `"memory"`. The default
    /// returns `"custom"` so impls don't have to override unless they
    /// want telemetry to know.
    fn system(&self) -> &'static str {
        "custom"
    }
}

/// Bundle of optional connection handles. Cheap to clone — all inner
/// types are reference-counted.
#[derive(Clone, Default)]
pub struct State {
    pg: Option<sqlx::PgPool>,
    redis: Option<Arc<redis::Client>>,
    storage: Option<Arc<dyn StorageProvider>>,
}

impl State {
    /// Attach a custom [`StorageProvider`]. Probes it immediately and
    /// returns the error if the probe fails — same contract as the
    /// other backends ("fail fast at boot").
    pub async fn with_storage<S: StorageProvider>(mut self, storage: S) -> Result<Self> {
        storage.probe().await?;
        self.storage = Some(Arc::new(storage));
        Ok(self)
    }

    /// Attach a pre-boxed provider without re-probing. Use when the
    /// caller has already done their own validation, or when injecting
    /// a mock from tests.
    pub fn set_storage(&mut self, storage: Arc<dyn StorageProvider>) {
        self.storage = Some(storage);
    }

    /// Whether a storage provider is wired.
    pub fn has_storage(&self) -> bool {
        self.storage.is_some()
    }

    /// Borrow the trait-object storage handle. Returns a `Config` error
    /// if no provider was wired (scaffold without `--with-storage`, or
    /// `STORAGE_BUCKET` unset in the generated init).
    pub fn storage(&self) -> Result<&Arc<dyn StorageProvider>> {
        self.storage.as_ref().ok_or_else(|| {
            Error::Config(
                "storage requested but no provider was wired at startup \
                 (scaffold with --with-storage to enable)"
                    .into(),
            )
        })
    }

    /// Build a `State` from environment variables. Tries each backend
    /// independently; missing env vars produce `None` fields, not
    /// errors. Connection failures DO error — if `DATABASE_URL` is set
    /// but unreachable, that's a deploy-time problem and the service
    /// should fail to start.
    ///
    /// Object storage is **not** initialized here — its concrete client
    /// type (opendal::Operator, AWS SDK, etc.) is scaffold-time opt-in,
    /// so this framework crate doesn't pull those deps. The scaffolded
    /// `main.rs` calls [`State::with_storage`] separately when
    /// `--with-storage` was used.
    pub async fn from_env() -> Result<Self> {
        let pg = match std::env::var("DATABASE_URL") {
            Ok(url) => {
                tracing::info!(target: "tonin::state", "connecting to postgres");
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(default_pg_max_conns())
                    .connect(&url)
                    .await
                    .map_err(|e| Error::Config(format!("postgres connect failed: {e}")))?;
                Some(pool)
            }
            Err(_) => None,
        };

        let redis = match std::env::var("REDIS_URL") {
            Ok(url) => {
                tracing::info!(target: "tonin::state", "connecting to redis");
                let client = redis::Client::open(url)
                    .map_err(|e| Error::Config(format!("redis client init: {e}")))?;
                // Eagerly verify reachability so a misconfigured cache fails fast.
                let mut conn = client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(|e| Error::Config(format!("redis connect failed: {e}")))?;
                let _: String = redis::cmd("PING")
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Config(format!("redis PING failed: {e}")))?;
                Some(Arc::new(client))
            }
            Err(_) => None,
        };

        Ok(Self {
            pg,
            redis,
            storage: None,
        })
    }

    /// Borrow the Postgres pool. Returns a `Config` error if
    /// `DATABASE_URL` was not set at boot.
    pub fn pg(&self) -> Result<&sqlx::PgPool> {
        self.pg.as_ref().ok_or_else(|| {
            Error::Config("postgres requested but DATABASE_URL was not set at startup".into())
        })
    }

    /// Whether the Postgres pool is available.
    pub fn has_pg(&self) -> bool {
        self.pg.is_some()
    }

    /// Borrow the Redis client. Returns a `Config` error if `REDIS_URL`
    /// was not set at boot.
    pub fn redis(&self) -> Result<&redis::Client> {
        self.redis.as_deref().ok_or_else(|| {
            Error::Config("redis requested but REDIS_URL was not set at startup".into())
        })
    }

    /// Whether the Redis client is available.
    pub fn has_redis(&self) -> bool {
        self.redis.is_some()
    }
}

/// Default max pool size. Overridable via `TONIN_PG_MAX_CONNECTIONS`.
fn default_pg_max_conns() -> u32 {
    std::env::var("TONIN_PG_MAX_CONNECTIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// In-memory mock implementing [`StorageProvider`]. Verifies the
    /// trait is implementable without the opendal dep — same shape any
    /// custom user impl would take.
    struct MockStorage {
        probes: AtomicUsize,
        probe_fails: bool,
    }

    #[async_trait]
    impl StorageProvider for MockStorage {
        async fn probe(&self) -> Result<()> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            if self.probe_fails {
                Err(Error::Config("mock probe failure".into()))
            } else {
                Ok(())
            }
        }
        fn system(&self) -> &'static str {
            "memory"
        }
    }

    #[tokio::test]
    async fn empty_state_when_no_env_vars() {
        // Guard against the test environment leaking these. If they
        // happen to be set on the dev box, skip — the assertion below
        // wouldn't hold but it's not a code bug.
        if std::env::var("DATABASE_URL").is_ok() || std::env::var("REDIS_URL").is_ok() {
            return;
        }
        let state = State::from_env().await.unwrap();
        assert!(!state.has_pg());
        assert!(!state.has_redis());
        assert!(!state.has_storage());
        assert!(state.pg().is_err());
        assert!(state.redis().is_err());
        assert!(state.storage().is_err());
    }

    #[tokio::test]
    async fn with_storage_runs_probe() {
        let state = State::default();
        let storage = MockStorage {
            probes: AtomicUsize::new(0),
            probe_fails: false,
        };
        let state = state.with_storage(storage).await.unwrap();
        assert!(state.has_storage());
        assert_eq!(state.storage().unwrap().system(), "memory");
    }

    #[tokio::test]
    async fn with_storage_propagates_probe_failure() {
        let state = State::default();
        let storage = MockStorage {
            probes: AtomicUsize::new(0),
            probe_fails: true,
        };
        match state.with_storage(storage).await {
            Ok(_) => panic!("expected probe failure to propagate"),
            Err(Error::Config(_)) => {}
            Err(other) => panic!("expected Config, got {other:?}"),
        }
    }
}
