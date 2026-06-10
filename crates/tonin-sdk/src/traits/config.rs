//! Dynamic application config capability.
//!
//! Load typed config values from any backend (env vars, file, etcd, GitHub
//! private repo, k8s ConfigMap, ...) behind a single trait. Sources are
//! composable via [`ChainedConfig`] for "file → env → etcd → defaults" style
//! fallback chains, and hot reload is exposed as a `tokio::sync::watch`
//! receiver.
//!
//! ## Distinction vs `SecretStore`
//!
//! `SecretStore` deals with credentials — narrow `String` API, telemetry
//! never records the key name, providers are expected to cache aggressively.
//! `Config` is for non-secret application config (DB pool sizes, feature
//! toggles, tuning knobs). Returns raw bytes that the caller deserializes
//! (typically via [`get_typed`]); keys ARE recorded as span attributes.
//!
//! ## Engines
//!
//! - **`EnvConfig`** — default, env-var-backed. Always available, no extra
//!   deps. `path = "db.pool.max"` → env var `<prefix>DB_POOL_MAX`.
//! - **`tonin-config-etcd`** — etcd v3 backed, native watch API for true
//!   server-pushed hot reload. TLS-friendly for the in-cluster etcd pattern.
//! - **`tonin-config-github`** — pull config from a private GitHub repo
//!   over the Contents API. Polls commit SHA on a configurable interval for
//!   reload.
//! - **`ChainedConfig`** — compose any of the above; first non-`None` wins.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;

use crate::Error;

/// Read structured config from a backend.
///
/// Implementations should make `get` cheap (cache lookups in memory, refresh
/// in the background) so handlers can call it on the hot path without
/// thinking. `watch` returns a `tokio::sync::watch` receiver that emits the
/// current value at subscribe time and again on every backend-observed
/// change; polling implementations honor `interval` as the polling cadence,
/// push implementations (etcd) ignore it.
#[async_trait]
pub trait Config: Send + Sync + 'static {
    /// Read the raw bytes at `path`. `None` means "no value present" — not
    /// an error. Caller distinguishes "missing" from "wrong type" by
    /// pattern-matching the option, then deserializing via [`get_typed`].
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, Error>;

    /// Subscribe to changes at `path`. The receiver emits the current
    /// value (or `None` if absent) immediately, then re-emits on every
    /// observed change. Drop the receiver to unsubscribe — implementations
    /// shut down their watcher when the last receiver goes away.
    ///
    /// `interval` is the polling cadence for backends that can't push
    /// (`github`, file-based engines). Push-based engines (`etcd`) ignore
    /// it and emit as soon as the backend signals a change.
    fn watch(
        &self,
        path: &str,
        interval: Duration,
    ) -> tokio::sync::watch::Receiver<Option<Vec<u8>>>;

    /// Span attribute `config.source` — `"env"`, `"file"`, `"etcd"`,
    /// `"github"`, `"chained"`, …
    fn source(&self) -> &'static str;
}

/// Typed accessor — JSON-deserialize the value at `path`. Most service
/// code uses this rather than the raw byte API.
///
/// Free function (not a trait method) so backends only have to implement
/// the small required surface, and so it works uniformly across any
/// `dyn Config` without needing a `Self: Sized` bound.
pub async fn get_typed<T: DeserializeOwned>(
    cfg: &(dyn Config + '_),
    path: &str,
) -> Result<Option<T>, Error> {
    match cfg.get(path).await? {
        Some(bytes) => {
            let v = serde_json::from_slice(&bytes).map_err(|e| {
                Error::CapabilityPermanent(format!("config '{path}' deserialize: {e}"))
            })?;
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/// Compose multiple sources into a fallback chain. The first source with a
/// non-`None` result wins. This is what `[config].engine = "chained"` builds
/// from the listed `sources` in `tonin.toml`.
///
/// `watch` subscribes to the **first** source that has the key at the time
/// of subscribe, falling back through the chain. A future enhancement could
/// re-evaluate the chain on any source change; today we keep it simple.
pub struct ChainedConfig {
    sources: Vec<Arc<dyn Config>>,
}

impl ChainedConfig {
    pub fn new(sources: Vec<Arc<dyn Config>>) -> Self {
        Self { sources }
    }
}

#[async_trait]
impl Config for ChainedConfig {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        for src in &self.sources {
            if let Some(v) = src.get(path).await? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn watch(
        &self,
        path: &str,
        interval: Duration,
    ) -> tokio::sync::watch::Receiver<Option<Vec<u8>>> {
        if let Some(first) = self.sources.first() {
            first.watch(path, interval)
        } else {
            let (_tx, rx) = tokio::sync::watch::channel(None);
            rx
        }
    }

    fn source(&self) -> &'static str {
        "chained"
    }
}

/// Default `Config` impl that reads from process env vars.
///
/// Path translation: `"db.pool.max_connections"` + prefix `"APP_"` →
/// env var `"APP_DB_POOL_MAX_CONNECTIONS"`. Dots become underscores, the
/// whole key is uppercased, and the configured prefix is prepended.
///
/// Env vars don't change at runtime in any meaningful way — `watch` emits
/// the current value once and never updates. Pair with a real engine via
/// [`ChainedConfig`] when you want live reload.
pub struct EnvConfig {
    pub prefix: String,
}

impl EnvConfig {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    fn env_key(&self, path: &str) -> String {
        let mut s = String::with_capacity(self.prefix.len() + path.len());
        s.push_str(&self.prefix);
        for ch in path.chars() {
            if ch == '.' {
                s.push('_');
            } else {
                s.push(ch.to_ascii_uppercase());
            }
        }
        s
    }
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self::new("")
    }
}

#[async_trait]
impl Config for EnvConfig {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        Ok(std::env::var(self.env_key(path))
            .ok()
            .map(String::into_bytes))
    }

    fn watch(
        &self,
        path: &str,
        _interval: Duration,
    ) -> tokio::sync::watch::Receiver<Option<Vec<u8>>> {
        let current = std::env::var(self.env_key(path))
            .ok()
            .map(String::into_bytes);
        // Hold onto the sender forever — env doesn't change, so we just
        // park the initial value and never emit again. `std::mem::forget`
        // keeps the channel alive for the receiver's lifetime.
        let (tx, rx) = tokio::sync::watch::channel(current);
        std::mem::forget(tx);
        rx
    }

    fn source(&self) -> &'static str {
        "env"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_key_translation_is_correct() {
        let c = EnvConfig::new("APP_");
        assert_eq!(c.env_key("db.pool.max"), "APP_DB_POOL_MAX");
        assert_eq!(c.env_key("feature.flag"), "APP_FEATURE_FLAG");
        assert_eq!(c.env_key(""), "APP_");
    }

    #[tokio::test]
    async fn env_get_returns_value_or_none() {
        // SAFETY: set_var is `unsafe` in edition-2024. Tests are
        // single-threaded for the process and the global env is the
        // unit under test, so the unsafe block is contained here.
        let key = "TONIN_TEST_CONFIG_VALUE_42";
        unsafe { std::env::set_var(key, "hello") };
        let c = EnvConfig::new("TONIN_TEST_CONFIG_");
        assert_eq!(c.get("value.42").await.unwrap(), Some(b"hello".to_vec()));
        assert!(c.get("missing.key").await.unwrap().is_none());
        unsafe { std::env::remove_var(key) };
    }

    #[tokio::test]
    async fn chained_falls_through_to_next_source() {
        let key = "TONIN_TEST_CHAINED_FALLBACK";
        unsafe { std::env::set_var(key, "from-env") };
        let first = Arc::new(EnvConfig::new("TONIN_TEST_EMPTY_"));
        let second = Arc::new(EnvConfig::new("TONIN_TEST_CHAINED_"));
        let chain = ChainedConfig::new(vec![first, second]);
        assert_eq!(
            chain.get("fallback").await.unwrap(),
            Some(b"from-env".to_vec())
        );
        assert_eq!(chain.source(), "chained");
        unsafe { std::env::remove_var(key) };
    }
}
