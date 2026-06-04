//! Generic coalescing wrapper for tonic-generated gRPC clients.
//!
//! [`CoalescingClient`] wraps any tonic-generated client `C` and provides:
//! - **Request coalescing** (singleflight) — default ON. Concurrent calls
//!   with identical keys share one upstream RPC.
//! - **Optional per-method TTL cache** — OFF unless configured via
//!   [`CoalescingClientBuilder::cache`]. Only `Ok` responses are cached.
//!
//! Call order per unary RPC:
//! `cache hit → (miss) coalescing → upstream → populate cache`
//!
//! ## Usage
//!
//! ```ignore
//! use tonin_client::client::CoalescingClient;
//! use tonin_client::cache::CacheConfig;
//! use std::time::Duration;
//!
//! // Default: coalescing ON, no cache.
//! let inner = AuthClient::connect("http://auth:50051").await?;
//! let client = CoalescingClient::new(inner);
//!
//! // With TTL cache on the Check method:
//! let client = CoalescingClient::builder(inner)
//!     .cache("Check", CacheConfig::new(Duration::from_millis(500), 1_000))
//!     .build();
//!
//! // Disable coalescing (e.g. for side-effecting write methods):
//! let client = CoalescingClient::builder(inner)
//!     .coalesce(false)
//!     .build();
//! ```
//!
//! ## Key derivation
//!
//! Default key: `(service_path, method_name, prost::encode_to_vec(request))`.
//! Any change to any request field — including a nested `consistency` field —
//! produces a different key, so only byte-for-byte identical requests coalesce.
//! Override via [`CoalescingClientBuilder::key_fn`] when responses also vary by
//! caller identity and the auth principal should be part of the key.
//!
//! ## Config precedence
//!
//! `tonin.toml` `[client]` fields are applied at code-generation time.
//! [`CoalescingClientBuilder`] overrides them at runtime; the builder always wins.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use tonic::{Response, Status};

use crate::cache::{CacheConfig, ResponseCache};
use crate::coalesce::{DefaultKeyFn, KeyFn, Singleflight};

/// Runtime configuration for a [`CoalescingClient`].
#[derive(Clone, Default)]
pub struct ClientConfig {
    /// Enable request coalescing. Default: `true`.
    pub coalesce: bool,
    /// Per-method TTL cache configs. Key = method name (e.g. `"Check"`).
    pub caches: HashMap<String, CacheConfig>,
}

impl ClientConfig {
    pub fn new() -> Self {
        Self {
            coalesce: true,
            caches: HashMap::new(),
        }
    }
}

/// Builder for [`CoalescingClient`].
pub struct CoalescingClientBuilder<C> {
    inner: C,
    coalesce: bool,
    caches: HashMap<String, CacheConfig>,
    key_fn: Arc<dyn KeyFn>,
}

impl<C> CoalescingClientBuilder<C> {
    fn new(inner: C) -> Self {
        Self {
            inner,
            coalesce: true,
            caches: HashMap::new(),
            key_fn: Arc::new(DefaultKeyFn),
        }
    }

    /// Enable or disable coalescing for all methods. Default: `true`.
    pub fn coalesce(mut self, enabled: bool) -> Self {
        self.coalesce = enabled;
        self
    }

    /// Add a TTL response cache for `method`. Off by default; must opt in.
    pub fn cache(mut self, method: impl Into<String>, cfg: CacheConfig) -> Self {
        self.caches.insert(method.into(), cfg);
        self
    }

    /// Override the key derivation function. The default includes only
    /// request bytes in the key. Supply a custom `KeyFn` when responses
    /// vary by caller identity (e.g. include the auth principal header).
    pub fn key_fn(mut self, f: impl KeyFn) -> Self {
        self.key_fn = Arc::new(f);
        self
    }

    pub fn build(self) -> CoalescingClient<C> {
        CoalescingClient {
            inner: self.inner,
            coalesce: self.coalesce,
            cache_configs: self.caches,
            key_fn: self.key_fn,
            singleflights: DashMap::new(),
            response_caches: DashMap::new(),
        }
    }
}

/// A coalescing + optional-cache wrapper around a tonic-generated client.
///
/// Call [`CoalescingClient::call`] from within generated method wrappers,
/// passing the encoded request bytes, service path, method name, and the
/// upstream closure. Coalescing and caching are handled transparently.
pub struct CoalescingClient<C> {
    /// The wrapped tonic-generated client.
    pub inner: C,
    coalesce: bool,
    cache_configs: HashMap<String, CacheConfig>,
    key_fn: Arc<dyn KeyFn>,
    /// Type-erased per-method singleflights: method → `Arc<Singleflight<R>>`.
    /// The key→R-type contract is enforced by the generated client: a given
    /// method name always maps to the same response type.
    singleflights: DashMap<String, Arc<dyn Any + Send + Sync>>,
    /// Type-erased per-method caches: method → `Arc<ResponseCache<R>>`.
    response_caches: DashMap<String, Arc<dyn Any + Send + Sync>>,
}

impl<C> CoalescingClient<C> {
    /// Wrap `inner` with default config (coalescing ON, no cache).
    pub fn new(inner: C) -> Self {
        CoalescingClientBuilder::new(inner).build()
    }

    /// Start a builder for granular per-method configuration.
    pub fn builder(inner: C) -> CoalescingClientBuilder<C> {
        CoalescingClientBuilder::new(inner)
    }

    /// Execute a coalesced (and optionally cached) unary call.
    ///
    /// - `service`: gRPC service path, e.g. `"auth.v1.AuthService"`.
    /// - `method`: method name, e.g. `"Check"`.
    /// - `req_bytes`: `prost::Message::encode_to_vec(&request)` — the
    ///   caller encodes once and passes the bytes for key derivation.
    /// - `upstream`: async closure that performs the actual tonic call and
    ///   returns `Result<Response<R>, Status>`.
    ///
    /// `R` must be `Clone + Send + Sync + 'static` so it can be shared
    /// across waiters and stored in the cache.
    ///
    /// **Streaming calls must not use this method.** Pass through to `self.inner` directly.
    pub async fn call<R, F, Fut>(
        &self,
        service: &str,
        method: &str,
        req_bytes: Vec<u8>,
        upstream: F,
    ) -> Result<Response<R>, Status>
    where
        R: Clone + Send + Sync + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Response<R>, Status>> + Send + 'static,
    {
        let key = self.key_fn.derive(service, method, &req_bytes);

        // 1. TTL cache lookup.
        if let Some(cache) = self.get_cache::<R>(method)
            && let Some(hit) = cache.get(&key)
        {
            tracing::debug!(method, "coalesce: cache hit");
            return Ok(Response::new(hit));
        }

        // 2. Coalescing (or direct call if disabled).
        let result: Result<R, Status> = if self.coalesce {
            let sf = self.get_or_create_sf::<R>(method);
            sf.run(key.clone(), service, method, async move {
                upstream().await.map(Response::into_inner)
            })
            .await
        } else {
            upstream().await.map(Response::into_inner)
        };

        // 3. Populate cache on Ok.
        if let Ok(ref value) = result
            && let Some(cache) = self.get_cache::<R>(method)
        {
            cache.insert(&key, value.clone());
        }

        result.map(Response::new)
    }

    // --- private helpers ---

    /// Get or atomically create the `Singleflight<R>` for `method`.
    ///
    /// The `DashMap::entry()` API holds a per-key shard lock for the
    /// duration of `or_insert_with`, so concurrent first-calls for the
    /// same method race safely: only one `Singleflight` is ever created.
    fn get_or_create_sf<R: Clone + Send + Sync + 'static>(
        &self,
        method: &str,
    ) -> Arc<Singleflight<R>> {
        let entry = self
            .singleflights
            .entry(method.to_string())
            .or_insert_with(|| Arc::new(Singleflight::<R>::new()) as Arc<dyn Any + Send + Sync>);

        entry
            .value()
            .clone()
            .downcast::<Singleflight<R>>()
            .expect("method always maps to the same response type")
    }

    /// Get the `ResponseCache<R>` for `method`, creating it on first access
    /// if a `CacheConfig` was supplied for this method.
    fn get_cache<R: Clone + Send + Sync + 'static>(
        &self,
        method: &str,
    ) -> Option<Arc<ResponseCache<R>>> {
        let cfg = self.cache_configs.get(method)?;

        let entry = self
            .response_caches
            .entry(method.to_string())
            .or_insert_with(|| {
                Arc::new(ResponseCache::<R>::new(cfg.clone())) as Arc<dyn Any + Send + Sync>
            });

        entry.value().clone().downcast::<ResponseCache<R>>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheConfig;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tonic::{Response, Status};

    // A minimal fake inner client for tests — holds a counter and a flag
    // controlling whether calls succeed.
    #[derive(Clone)]
    struct FakeClient {
        counter: Arc<AtomicU32>,
        fail: bool,
    }

    impl FakeClient {
        fn new() -> Self {
            Self {
                counter: Arc::new(AtomicU32::new(0)),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                counter: Arc::new(AtomicU32::new(0)),
                fail: true,
            }
        }
        async fn check(&self, _: &str) -> Result<Response<String>, Status> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            if self.fail {
                Err(Status::internal("injected failure"))
            } else {
                Ok(Response::new("ok".into()))
            }
        }
    }

    #[tokio::test]
    async fn concurrent_identical_calls_share_one_upstream() {
        let fake = FakeClient::new();
        let counter = fake.counter.clone();
        let client = CoalescingClient::new(fake.clone());

        let client = Arc::new(client);
        let mut handles = Vec::new();
        for _ in 0..6 {
            let c = client.inner.clone();
            let cl = Arc::clone(&client);
            handles.push(tokio::spawn(async move {
                cl.call::<String, _, _>("svc", "Check", b"request".to_vec(), move || {
                    let c = c.clone();
                    async move { c.check("input").await }
                })
                .await
            }));
        }

        let results = futures_util::future::join_all(handles).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        for r in results {
            assert_eq!(r.unwrap().unwrap().into_inner(), "ok");
        }
    }

    #[tokio::test]
    async fn different_request_bytes_not_coalesced() {
        let fake = FakeClient::new();
        let counter = fake.counter.clone();
        let client = Arc::new(CoalescingClient::new(fake.clone()));

        let mut handles = Vec::new();
        for i in 0u8..4 {
            let c = fake.clone();
            let cl = Arc::clone(&client);
            handles.push(tokio::spawn(async move {
                cl.call::<String, _, _>("svc", "Check", vec![i], move || {
                    let c = c.clone();
                    async move { c.check("x").await }
                })
                .await
            }));
        }
        futures_util::future::join_all(handles).await;
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn error_shared_not_cached_next_call_retries() {
        let fake = FakeClient::failing();
        let counter = fake.counter.clone();
        let client = Arc::new(CoalescingClient::new(fake.clone()));

        // First flight: 3 concurrent callers all get the error.
        let mut handles = Vec::new();
        for _ in 0..3 {
            let c = fake.clone();
            let cl = Arc::clone(&client);
            handles.push(tokio::spawn(async move {
                cl.call::<String, _, _>("svc", "Check", b"k".to_vec(), move || {
                    let c = c.clone();
                    async move { c.check("x").await }
                })
                .await
            }));
        }
        let results = futures_util::future::join_all(handles).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        for r in results {
            assert!(r.unwrap().is_err());
        }

        // Second call: fresh attempt (error was not cached).
        // Use a non-failing client with the same singleflight map.
        let ok_fake = FakeClient::new();
        let ok_counter = ok_fake.counter.clone();
        let result = client
            .call::<String, _, _>("svc", "Check", b"k".to_vec(), move || {
                let c = ok_fake.clone();
                async move { c.check("x").await }
            })
            .await;
        assert_eq!(ok_counter.load(Ordering::SeqCst), 1);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn ttl_cache_hit_within_ttl_no_upstream() {
        let fake = FakeClient::new();
        let counter = fake.counter.clone();
        let client = CoalescingClient::builder(fake.clone())
            .cache("Check", CacheConfig::new(Duration::from_secs(10), 100))
            .build();

        // First call: upstream hit.
        let c = fake.clone();
        client
            .call::<String, _, _>("svc", "Check", b"k".to_vec(), move || {
                let c = c.clone();
                async move { c.check("x").await }
            })
            .await
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Second call within TTL: cache hit, no upstream.
        let c = fake.clone();
        let result = client
            .call::<String, _, _>("svc", "Check", b"k".to_vec(), move || {
                let c = c.clone();
                async move { c.check("x").await }
            })
            .await
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1); // still 1
        assert_eq!(result.into_inner(), "ok");
    }

    #[tokio::test]
    async fn ttl_cache_miss_after_expiry() {
        let fake = FakeClient::new();
        let counter = fake.counter.clone();
        let client = CoalescingClient::builder(fake.clone())
            .cache("Check", CacheConfig::new(Duration::from_millis(10), 100))
            .build();

        let c = fake.clone();
        client
            .call::<String, _, _>("svc", "Check", b"k".to_vec(), move || {
                let c = c.clone();
                async move { c.check("x").await }
            })
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(30)).await;

        let c = fake.clone();
        client
            .call::<String, _, _>("svc", "Check", b"k".to_vec(), move || {
                let c = c.clone();
                async move { c.check("x").await }
            })
            .await
            .unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn coalescing_disabled_passes_through() {
        let fake = FakeClient::new();
        let counter = fake.counter.clone();
        let client = Arc::new(
            CoalescingClient::builder(fake.clone())
                .coalesce(false)
                .build(),
        );

        let mut handles = Vec::new();
        for _ in 0..4 {
            let c = fake.clone();
            let cl = Arc::clone(&client);
            handles.push(tokio::spawn(async move {
                cl.call::<String, _, _>("svc", "Check", b"k".to_vec(), move || {
                    let c = c.clone();
                    async move { c.check("x").await }
                })
                .await
            }));
        }
        futures_util::future::join_all(handles).await;
        // No coalescing: all 4 go upstream.
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }
}
