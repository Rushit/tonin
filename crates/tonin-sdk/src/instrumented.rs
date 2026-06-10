//! `Instrumented<T>` — the single decorator that gives every capability
//! call a span, metric, and slow-op WARN log.
//!
//! Service authors never see this directly. Phase 5 will wrap each
//! capability returned from `Service` accessors in `Instrumented`, so
//! `svc.cache().get("k")` automatically gets:
//!
//! - a `cache.get` span as a child of the current request span,
//! - a `cache_op_duration_ms` histogram observation,
//! - a `cache_op_total` counter increment,
//! - a WARN log if the call exceeds `SlowThresholds::cache_op` or fails.
//!
//! New backend impls (`tonin-redis`, future `tonin-postgres`) get
//! all of this for free — they only implement the trait methods.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_core::Stream;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::telemetry::capability_metrics::{
    SlowThresholds, record_cache_op, record_messaging_publish,
};
use crate::telemetry::propagate::{extract_context_from_map, inject_current_context_map};

use crate::Error;
use crate::traits::{
    Cache, Config, Database, DeliveredMessage, EventBus, MessageId, SecretStore, SubscribeOptions,
    Subscription,
};

/// Decorator that wraps any capability impl with telemetry. Cheap to
/// clone — internal state is an `Arc` to the inner impl plus a small
/// `SlowThresholds`.
///
/// ```
/// # use std::sync::Arc;
/// # use std::time::Duration;
/// # use async_trait::async_trait;
/// # use tonin_sdk::{instrumented::Instrumented, traits::Cache, Error};
/// struct MockCache;
/// #[async_trait]
/// impl Cache for MockCache {
///     fn system(&self) -> &'static str { "mock" }
///     async fn get(&self, _k: &str) -> Result<Option<Vec<u8>>, Error> {
///         Ok(Some(b"hi".to_vec()))
///     }
///     async fn set(&self, _k: &str, _v: &[u8], _t: Option<Duration>) -> Result<(), Error> { Ok(()) }
///     async fn del(&self, _k: &str) -> Result<(), Error> { Ok(()) }
///     async fn set_nx(&self, _k: &str, _v: &[u8], _t: Option<Duration>) -> Result<bool, Error> { Ok(true) }
/// }
///
/// # tokio_test::block_on(async {
/// let cache = Instrumented::with_defaults(Arc::new(MockCache));
/// let v = Cache::get(&cache, "hello").await.unwrap();
/// assert_eq!(v, Some(b"hi".to_vec()));
/// # });
/// ```
pub struct Instrumented<T: ?Sized> {
    inner: Arc<T>,
    slow: SlowThresholds,
}

impl<T: ?Sized> Clone for Instrumented<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            slow: self.slow.clone(),
        }
    }
}

impl<T: ?Sized> Instrumented<T> {
    pub fn new(inner: Arc<T>, slow: SlowThresholds) -> Self {
        Self { inner, slow }
    }

    /// Use the framework's default thresholds — handy for tests and the
    /// Phase 5 wiring path when no `[telemetry.slow]` was provided.
    pub fn with_defaults(inner: Arc<T>) -> Self {
        Self::new(inner, SlowThresholds::default())
    }
}

// ---------- Cache impl ----------

#[async_trait]
impl<T: Cache + ?Sized> Cache for Instrumented<T> {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        let span = tracing::info_span!(
            "cache.get",
            cache.system = self.inner.system(),
            cache.op = "get",
            cache.key.hash = key_hash(key),
            cache.hit = tracing::field::Empty,
        );
        let _enter = span.enter();
        let start = Instant::now();
        let result = self.inner.get(key).await;
        let elapsed = start.elapsed();
        let outcome = match &result {
            Ok(Some(_)) => "hit",
            Ok(None) => "miss",
            Err(_) => "error",
        };
        if let Ok(opt) = &result {
            Span::current().record("cache.hit", opt.is_some());
        }
        record_cache_op(self.inner.system(), "get", outcome, elapsed);
        slow_or_error_log(elapsed, self.slow.cache_op, &result, "cache.get", key);
        result
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), Error> {
        let span = tracing::info_span!(
            "cache.set",
            cache.system = self.inner.system(),
            cache.op = "set",
            cache.key.hash = key_hash(key),
            ttl_ms = ttl.map(|d| d.as_millis() as u64),
        );
        let _enter = span.enter();
        let start = Instant::now();
        let result = self.inner.set(key, value, ttl).await;
        let elapsed = start.elapsed();
        let outcome = if result.is_ok() { "ok" } else { "error" };
        record_cache_op(self.inner.system(), "set", outcome, elapsed);
        slow_or_error_log(elapsed, self.slow.cache_op, &result, "cache.set", key);
        result
    }

    async fn del(&self, key: &str) -> Result<(), Error> {
        let span = tracing::info_span!(
            "cache.del",
            cache.system = self.inner.system(),
            cache.op = "del",
            cache.key.hash = key_hash(key),
        );
        let _enter = span.enter();
        let start = Instant::now();
        let result = self.inner.del(key).await;
        let elapsed = start.elapsed();
        let outcome = if result.is_ok() { "ok" } else { "error" };
        record_cache_op(self.inner.system(), "del", outcome, elapsed);
        slow_or_error_log(elapsed, self.slow.cache_op, &result, "cache.del", key);
        result
    }

    async fn set_nx(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool, Error> {
        let span = tracing::info_span!(
            "cache.set_nx",
            cache.system = self.inner.system(),
            cache.op = "setnx",
            cache.key.hash = key_hash(key),
            ttl_ms = ttl.map(|d| d.as_millis() as u64),
            cache.created = tracing::field::Empty,
        );
        let _enter = span.enter();
        let start = Instant::now();
        let result = self.inner.set_nx(key, value, ttl).await;
        let elapsed = start.elapsed();
        let outcome = match &result {
            Ok(true) => "ok",
            Ok(false) => "exists",
            Err(_) => "error",
        };
        if let Ok(created) = &result {
            Span::current().record("cache.created", *created);
        }
        record_cache_op(self.inner.system(), "setnx", outcome, elapsed);
        slow_or_error_log(elapsed, self.slow.cache_op, &result, "cache.set_nx", key);
        result
    }

    fn system(&self) -> &'static str {
        self.inner.system()
    }
}

// ---------- Database impl ----------
//
// Pass-through. The trait has no methods worth wrapping (url() is sync
// and infallible; pool() is a borrow). Query spans come from the user's
// chosen DB client (sqlx, sea-orm), which emits OTel spans via the
// global tracer that `crate::telemetry::init` installs.

#[async_trait]
impl<T: Database + ?Sized> Database for Instrumented<T> {
    fn url(&self) -> &str {
        self.inner.url()
    }
    fn system(&self) -> &'static str {
        self.inner.system()
    }
}

// ---------- SecretStore impl ----------

#[async_trait]
impl<T: SecretStore + ?Sized> SecretStore for Instrumented<T> {
    async fn get(&self, key: &str) -> Result<String, Error> {
        // The key name is intentionally NOT a span attribute — secret
        // names can be sensitive. Only the provider is recorded.
        let span = tracing::info_span!("secret.get", secret.provider = self.inner.provider(),);
        let _enter = span.enter();
        let start = Instant::now();
        let result = self.inner.get(key).await;
        let elapsed = start.elapsed();
        // No metric for SecretStore in Phase 3 — it's hot-path-cached
        // and uninteresting volumetrically. Add later if a backend
        // makes per-call network hops.
        if result.is_err() {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                provider = self.inner.provider(),
                "secret.get failed",
            );
        }
        result
    }

    fn provider(&self) -> &'static str {
        self.inner.provider()
    }
}

// ---------- Config impl ----------

#[async_trait]
impl<T: Config + ?Sized> Config for Instrumented<T> {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        // Path IS recorded as a span attribute — unlike SecretStore::get
        // where the key name can be sensitive, config paths are application
        // structure (db.pool.max, feature.flag) and are safe to record.
        let span = tracing::info_span!(
            "config.get",
            config.source = self.inner.source(),
            config.path = path,
        );
        let _enter = span.enter();
        let start = Instant::now();
        let result = self.inner.get(path).await;
        let elapsed = start.elapsed();
        if result.is_err() {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                source = self.inner.source(),
                path,
                "config.get failed",
            );
        }
        result
    }

    fn watch(
        &self,
        path: &str,
        interval: std::time::Duration,
    ) -> tokio::sync::watch::Receiver<Option<Vec<u8>>> {
        // No span around watch() — the channel lives much longer than
        // any single span and emits a stream of values; instrumenting
        // each emission lives in the consumer's processing loop.
        self.inner.watch(path, interval)
    }

    fn source(&self) -> &'static str {
        self.inner.source()
    }
}

// ---------- EventBus impl ----------

#[async_trait]
impl<T: EventBus + ?Sized> EventBus for Instrumented<T> {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<MessageId, Error> {
        let mut headers = HashMap::new();
        inject_current_context_map(&mut headers);
        self.publish_inner(subject, payload, headers).await
    }

    async fn publish_with_headers(
        &self,
        subject: &str,
        payload: &[u8],
        mut headers: HashMap<String, String>,
    ) -> Result<MessageId, Error> {
        // Only inject if the caller didn't already provide one — caller-
        // supplied `traceparent` wins (rare, but matters for testing).
        if !headers.contains_key("traceparent") {
            inject_current_context_map(&mut headers);
        }
        self.publish_inner(subject, payload, headers).await
    }

    async fn subscribe(
        &self,
        subject_pattern: &str,
        group: &str,
        opts: SubscribeOptions,
    ) -> Result<Subscription, Error> {
        let inner_sub = self.inner.subscribe(subject_pattern, group, opts).await?;
        let system: &'static str = self.inner.system();
        let group_owned = group.to_string();
        let subject_owned = subject_pattern.to_string();
        Ok(Subscription::new(InstrumentedSubscription {
            inner: inner_sub,
            system,
            group: group_owned,
            subject: subject_owned,
        }))
    }

    fn system(&self) -> &'static str {
        self.inner.system()
    }
}

impl<T: EventBus + ?Sized> Instrumented<T> {
    async fn publish_inner(
        &self,
        subject: &str,
        payload: &[u8],
        headers: HashMap<String, String>,
    ) -> Result<MessageId, Error> {
        let span = tracing::info_span!(
            "messaging.publish",
            messaging.system = self.inner.system(),
            messaging.destination.name = subject,
            messaging.destination.kind = "topic",
            messaging.message.body.size = payload.len(),
        );
        let _enter = span.enter();
        let start = Instant::now();
        let result = self
            .inner
            .publish_with_headers(subject, payload, headers)
            .await;
        let elapsed = start.elapsed();
        let outcome = if result.is_ok() { "ok" } else { "error" };
        record_messaging_publish(self.inner.system(), subject, outcome, elapsed);
        if elapsed > self.slow.messaging_publish || result.is_err() {
            tracing::warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                subject,
                error = result.as_ref().err().map(|e| e.to_string()),
                "slow or failing messaging.publish",
            );
        }
        result
    }
}

/// Wraps the backend `Subscription` and creates a `messaging.process`
/// span per yielded message. The span's parent is the publisher's
/// context, extracted from `msg.headers`. If the header is missing or
/// malformed, the span becomes a new root — never grafted onto
/// `Span::current()`, which would lie about causality.
struct InstrumentedSubscription {
    inner: Subscription,
    system: &'static str,
    group: String,
    subject: String,
}

impl Stream for InstrumentedSubscription {
    type Item = DeliveredMessage;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(msg)) => {
                // Emit messaging.process parented to the publisher's
                // context. Consumer handlers create their own spans;
                // those are linked to this trace via the propagator if
                // the handler enters this span. We don't enter it here
                // — the consumer owns processing-time accounting.
                let span = tracing::info_span!(
                    "messaging.process",
                    messaging.system = this.system,
                    messaging.destination.name = %msg.subject,
                    messaging.consumer.id = %this.group,
                    messaging.message.id = %msg.id,
                    messaging.delivery_attempt = msg.delivery_attempt,
                    subscription.subject_pattern = %this.subject,
                );
                let parent_cx = extract_context_from_map(&msg.headers);
                span.set_parent(parent_cx);
                drop(span);
                Poll::Ready(Some(msg))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------- helpers ----------

fn slow_or_error_log<T>(
    elapsed: Duration,
    threshold: Duration,
    result: &Result<T, Error>,
    op: &'static str,
    key_for_hash: &str,
) {
    if elapsed > threshold || result.is_err() {
        tracing::warn!(
            elapsed_ms = elapsed.as_millis() as u64,
            key.hash = key_hash(key_for_hash),
            error = result.as_ref().err().map(|e| e.to_string()),
            "slow or failing {op}",
            op = op,
        );
    }
}

/// Stable, low-cardinality hash. Not cryptographic — bounded label
/// cardinality, that's all.
fn key_hash(key: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}
