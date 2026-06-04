//! Request coalescing (singleflight) for outbound gRPC calls.
//!
//! Concurrent calls with an identical key share a single upstream RPC; all
//! waiters receive a clone of the same `Result`. After the flight resolves
//! the entry is removed — this is deduplication, not caching. Errors are
//! shared within a flight but never stored; the next call starts fresh.
//!
//! ## Async correctness
//!
//! No lock is held across `.await`. The pattern is:
//! 1. Lock → look up or insert a `Shared` future → unlock.
//! 2. Clone the handle outside the lock.
//! 3. Await the handle.
//!
//! `futures_util::future::Shared` keeps polling while at least one handle
//! is alive, so dropping a waiter before resolution does not cancel the
//! flight for the others (cancellation-safe for the "all callers wait"
//! pattern).
//!
//! ## Tracing
//!
//! Each call records a `coalesce` span with:
//! - `flight.role` = `"driver"` (first caller) or `"waiter"` (joined).
//! - `flight.key_hash` = stable hex of the key bytes (safe to log; never
//!   contains raw request data).
//!
//! The driver's trace context is propagated to the upstream RPC via the
//! existing [`crate::propagate`] module. Waiters show a near-zero-latency
//! `coalesce` span in their trace; correlate across traces by `flight.key_hash`.
//!
//! ## Key extensibility
//!
//! The key is `Vec<u8>`. The default derivation is
//! `(service_path, method_name, request.encode_to_vec())` — a change in any
//! request field produces a different key, so requests differing only in a
//! nested field (e.g. a consistency parameter) never coalesce. Callers can
//! supply a custom [`KeyFn`] to include additional context (e.g. auth principal)
//! when responses vary per caller.

use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use dashmap::DashMap;
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use tonic::Status;
use tracing::Instrument;

/// A function that derives a coalescing key from raw request bytes and tonic
/// metadata. The default implementation (`DefaultKeyFn`) returns the bytes
/// unchanged. Supply a custom `KeyFn` to include additional context (e.g.
/// an auth-principal header) in the key.
pub trait KeyFn: Send + Sync + 'static {
    fn derive(&self, service: &str, method: &str, request_bytes: &[u8]) -> Vec<u8>;
}

/// Default key: `service + "\x00" + method + "\x00" + request_bytes`.
/// A change in any byte of the request — including nested fields — produces
/// a different key.
pub struct DefaultKeyFn;

impl KeyFn for DefaultKeyFn {
    fn derive(&self, service: &str, method: &str, request_bytes: &[u8]) -> Vec<u8> {
        let mut key =
            Vec::with_capacity(service.len() + 1 + method.len() + 1 + request_bytes.len());
        key.extend_from_slice(service.as_bytes());
        key.push(0);
        key.extend_from_slice(method.as_bytes());
        key.push(0);
        key.extend_from_slice(request_bytes);
        key
    }
}

// Newtype so Vec<u8> is usable as a DashMap key.
#[derive(Clone, PartialEq, Eq)]
struct FlightKey(Vec<u8>);

impl Hash for FlightKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// Stable hex of the key for tracing — never the full payload.
pub fn key_hash_hex(key: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    let v = std::hash::Hasher::finish(&h);
    format!("{v:016x}")
}

type Flight<R> = Shared<BoxFuture<'static, Result<R, Status>>>;

/// In-flight map for a single service+method pair. Generic over the
/// decoded response type `R` (must be `Clone + Send + Sync + 'static`).
///
/// ## Why `Arc<DashMap<...>>`
///
/// `DashMap::clone()` (v6) performs a DEEP copy of all entries, acquiring
/// read locks on every shard. Calling `.clone()` inside a `DashMap::entry()`
/// arm (which holds a write lock on one shard) would deadlock on that shard.
/// Wrapping in `Arc` makes `.clone()` a cheap reference-count bump with no
/// lock, and ensures the cleanup closure removes from the SAME map instance.
pub struct Singleflight<R> {
    in_flight: Arc<DashMap<FlightKey, Flight<R>>>,
}

impl<R: Clone + Send + Sync + 'static> Singleflight<R> {
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(DashMap::new()),
        }
    }

    /// Call `f` for `key`, or join an existing in-flight call with the
    /// same key. `f` is only invoked once per concurrent group.
    ///
    /// `service` and `method` are used only for the tracing span;
    /// `key` is the pre-derived byte key from your [`KeyFn`].
    pub async fn run<F>(&self, key: Vec<u8>, service: &str, method: &str, f: F) -> Result<R, Status>
    where
        F: Future<Output = Result<R, Status>> + Send + 'static,
    {
        let fkey = FlightKey(key.clone());
        let hash = key_hash_hex(&key);

        // --- lock phase (no .await inside) ---
        let (handle, role) = {
            // DashMap entry() gives us a shard-level lock for this key.
            use dashmap::mapref::entry::Entry;
            match self.in_flight.entry(fkey.clone()) {
                Entry::Occupied(e) => (e.get().clone(), "waiter"),
                Entry::Vacant(e) => {
                    // We are the driver. Build + share the upstream future.
                    // `Arc::clone` is O(1) and acquires no locks — safe to
                    // call while holding the VacantEntry shard lock.
                    let map = Arc::clone(&self.in_flight);
                    let fkey2 = fkey.clone();
                    let shared: Flight<R> = async move {
                        let result = f.await;
                        map.remove(&fkey2);
                        result
                    }
                    .boxed()
                    .shared();
                    e.insert(shared.clone());
                    (shared, "driver")
                }
            }
        };
        // --- shard lock released; await outside ---

        let span = tracing::debug_span!(
            "coalesce",
            flight.role = role,
            flight.key_hash = %hash,
            otel.name = format!("{service}/{method}"),
        );
        handle.instrument(span).await
    }

    /// Number of in-flight requests. Useful in tests.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }
}

impl<R: Clone + Send + Sync + 'static> Default for Singleflight<R> {
    fn default() -> Self {
        Self::new()
    }
}

// Arc::clone is O(1) — Singleflight instances share the same underlying map.
impl<R: Clone + Send + Sync + 'static> Clone for Singleflight<R> {
    fn clone(&self) -> Self {
        Self {
            in_flight: Arc::clone(&self.in_flight),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn concurrent_identical_keys_share_one_call() {
        let sf: Arc<Singleflight<String>> = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let sf = sf.clone();
            let counter = counter.clone();
            handles.push(tokio::spawn(async move {
                sf.run(
                    b"svc\x00method\x00req".to_vec(),
                    "svc",
                    "method",
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Ok::<String, Status>("result".into())
                    },
                )
                .await
            }));
        }

        let results: Vec<_> = futures_util::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.expect("task panicked"))
            .collect();

        // Exactly one upstream call.
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        // All waiters received the same Ok result.
        for r in results {
            assert_eq!(r.unwrap(), "result");
        }
        // Entry cleaned up after flight.
        assert_eq!(sf.in_flight_count(), 0);
    }

    #[tokio::test]
    async fn different_keys_are_not_coalesced() {
        let sf: Arc<Singleflight<String>> = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for i in 0u8..4 {
            let sf = sf.clone();
            let counter = counter.clone();
            // Each request differs in one byte.
            let key = vec![i];
            handles.push(tokio::spawn(async move {
                sf.run(key, "svc", "method", async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok::<String, Status>(format!("result-{i}"))
                })
                .await
            }));
        }

        futures_util::future::join_all(handles).await;
        // Every distinct key drove its own call.
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn differing_only_in_nested_field_not_coalesced() {
        // Simulates two requests that are identical except for a consistency field.
        let sf: Arc<Singleflight<u32>> = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicU32::new(0));

        let key_a = b"req\x01consistency=strong".to_vec();
        let key_b = b"req\x01consistency=eventual".to_vec();

        let sf_a = sf.clone();
        let c_a = counter.clone();
        let h_a = tokio::spawn(async move {
            sf_a.run(key_a, "svc", "m", async move {
                c_a.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok::<u32, Status>(1)
            })
            .await
        });

        let sf_b = sf.clone();
        let c_b = counter.clone();
        let h_b = tokio::spawn(async move {
            sf_b.run(key_b, "svc", "m", async move {
                c_b.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok::<u32, Status>(2)
            })
            .await
        });

        let (ra, rb) = tokio::join!(h_a, h_b);
        assert_eq!(ra.unwrap().unwrap(), 1);
        assert_eq!(rb.unwrap().unwrap(), 2);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn error_is_shared_but_not_cached() {
        let sf: Arc<Singleflight<String>> = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicU32::new(0));

        // First flight: all waiters get the error.
        let mut handles = Vec::new();
        for _ in 0..3 {
            let sf = sf.clone();
            let counter = counter.clone();
            handles.push(tokio::spawn(async move {
                sf.run(b"key".to_vec(), "s", "m", async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Err::<String, Status>(Status::internal("boom"))
                })
                .await
            }));
        }
        let results = futures_util::future::join_all(handles).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1); // one upstream call
        for r in results {
            assert!(r.unwrap().is_err()); // all got the error
        }

        // Second call after the error: fresh upstream attempt (not cached).
        let counter2 = counter.clone();
        let result = sf
            .run(b"key".to_vec(), "s", "m", async move {
                counter2.fetch_add(1, Ordering::SeqCst);
                Ok::<String, Status>("retry ok".into())
            })
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 2); // second upstream call
        assert_eq!(result.unwrap(), "retry ok");
    }

    #[tokio::test]
    async fn dropped_waiter_does_not_break_others() {
        let sf: Arc<Singleflight<String>> = Arc::new(Singleflight::new());

        // Spawn two waiters; drop one immediately after spawning.
        let sf2 = sf.clone();
        let survivor = tokio::spawn(async move {
            sf2.run(b"k".to_vec(), "s", "m", async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok::<String, Status>("alive".into())
            })
            .await
        });

        let sf3 = sf.clone();
        let doomed = tokio::spawn(async move {
            sf3.run(b"k".to_vec(), "s", "m", async {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok::<String, Status>("alive".into())
            })
            .await
        });

        // Give them a tick to launch, then abort one.
        tokio::time::sleep(Duration::from_millis(5)).await;
        doomed.abort();

        // Survivor still gets the result.
        let r = survivor.await.expect("task panicked");
        assert_eq!(r.unwrap(), "alive");
    }
}
