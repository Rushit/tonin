//! Runtime configuration loaded from environment variables.

use std::time::Duration;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    /// Port the proxy listens on (default: 6565).
    pub port: u16,
    /// Upstream gRPC address (e.g. `http://localhost:50051`).
    pub upstream: String,
    /// Per-method cache TTL. `Duration::ZERO` disables caching.
    pub cache_ttl: Duration,
    /// Maximum entries in the response cache.
    pub cache_capacity: usize,
    /// Maximum upstream retry attempts (0 = no retries).
    pub retry_max: u32,
    /// Error-rate threshold [0.0, 1.0] that trips the circuit breaker.
    pub breaker_threshold: f64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let port = env_u16("TONIN_PROXY_PORT", 6565)?;
        let upstream = std::env::var("TONIN_PROXY_UPSTREAM")
            .context("TONIN_PROXY_UPSTREAM must be set (e.g. http://localhost:50051)")?;
        if upstream.trim().is_empty() {
            anyhow::bail!("TONIN_PROXY_UPSTREAM must not be empty");
        }
        let cache_ttl_ms = env_u64("TONIN_PROXY_CACHE_TTL_MS", 0)?;
        let cache_capacity = env_usize("TONIN_PROXY_CACHE_CAPACITY", 1_000)?;
        let retry_max = env_u32("TONIN_PROXY_RETRY_MAX", 3)?;
        let breaker_threshold = env_f64("TONIN_PROXY_BREAKER_THRESHOLD", 0.5)?;

        Ok(Self {
            port,
            upstream,
            cache_ttl: Duration::from_millis(cache_ttl_ms),
            cache_capacity,
            retry_max,
            breaker_threshold,
        })
    }

    pub fn cache_enabled(&self) -> bool {
        !self.cache_ttl.is_zero() && self.cache_capacity > 0
    }
}

fn env_u16(key: &str, default: u16) -> Result<u16> {
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("{key} must be a valid port number, got {v:?}")),
        Err(_) => Ok(default),
    }
}

fn env_u32(key: &str, default: u32) -> Result<u32> {
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("{key} must be a u32, got {v:?}")),
        Err(_) => Ok(default),
    }
}

fn env_u64(key: &str, default: u64) -> Result<u64> {
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("{key} must be a u64, got {v:?}")),
        Err(_) => Ok(default),
    }
}

fn env_usize(key: &str, default: usize) -> Result<usize> {
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("{key} must be a usize, got {v:?}")),
        Err(_) => Ok(default),
    }
}

fn env_f64(key: &str, default: f64) -> Result<f64> {
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("{key} must be a float, got {v:?}")),
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialise all env-mutating tests so parallel test threads don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_when_only_upstream_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK ensures no concurrent env mutation in this process.
        unsafe {
            std::env::set_var("TONIN_PROXY_UPSTREAM", "http://localhost:50051");
            for key in &[
                "TONIN_PROXY_PORT",
                "TONIN_PROXY_CACHE_TTL_MS",
                "TONIN_PROXY_CACHE_CAPACITY",
                "TONIN_PROXY_RETRY_MAX",
                "TONIN_PROXY_BREAKER_THRESHOLD",
            ] {
                std::env::remove_var(key);
            }
        }
        let cfg = Config::from_env().unwrap();
        assert_eq!(cfg.port, 6565);
        assert_eq!(cfg.upstream, "http://localhost:50051");
        assert!(!cfg.cache_enabled(), "cache off by default (ttl=0)");
        assert_eq!(cfg.retry_max, 3);
        assert!((cfg.breaker_threshold - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn cache_enabled_when_ttl_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK ensures no concurrent env mutation in this process.
        unsafe {
            std::env::set_var("TONIN_PROXY_UPSTREAM", "http://localhost:50051");
            std::env::set_var("TONIN_PROXY_CACHE_TTL_MS", "500");
        }
        let cfg = Config::from_env().unwrap();
        assert!(cfg.cache_enabled());
        unsafe { std::env::remove_var("TONIN_PROXY_CACHE_TTL_MS") };
    }

    #[test]
    fn missing_upstream_is_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK ensures no concurrent env mutation in this process.
        unsafe { std::env::remove_var("TONIN_PROXY_UPSTREAM") };
        assert!(Config::from_env().is_err());
    }

    #[test]
    fn empty_upstream_is_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK ensures no concurrent env mutation in this process.
        unsafe { std::env::set_var("TONIN_PROXY_UPSTREAM", "   ") };
        assert!(Config::from_env().is_err());
        unsafe { std::env::remove_var("TONIN_PROXY_UPSTREAM") };
    }
}
