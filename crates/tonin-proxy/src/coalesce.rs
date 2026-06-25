//! Singleflight coalescing keyed on raw gRPC path + body bytes.
//!
//! Concurrent calls with the same (path, body) share one upstream RPC.
//! Errors are shared within the flight but never persisted — the next
//! distinct call starts fresh regardless of prior outcomes.
//!
//! No lock is held across `.await`. Pattern:
//! 1. Lock → look up or insert a `Shared` future → unlock.
//! 2. Clone the handle.
//! 3. Await the handle.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};
use tracing::Instrument;

type Flight = Shared<BoxFuture<'static, Result<Bytes, String>>>;

#[derive(Clone, PartialEq, Eq)]
struct FlightKey(Vec<u8>);

impl Hash for FlightKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

pub fn key_hash_hex(key: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    format!("{:016x}", std::hash::Hasher::finish(&h))
}

/// Builds the coalescing key: `path + "\x00" + body`.
pub fn make_key(path: &str, body: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(path.len() + 1 + body.len());
    k.extend_from_slice(path.as_bytes());
    k.push(0);
    k.extend_from_slice(body);
    k
}

pub struct Singleflight {
    in_flight: Arc<DashMap<FlightKey, Flight>>,
}

impl Singleflight {
    pub fn new() -> Self {
        Self {
            in_flight: Arc::new(DashMap::new()),
        }
    }

    /// Drive `f` for `key`, or join an existing in-flight call.
    /// `path` is only used for the tracing span name.
    pub async fn run<F>(&self, key: Vec<u8>, path: &str, f: F) -> Result<Bytes, String>
    where
        F: std::future::Future<Output = Result<Bytes, String>> + Send + 'static,
    {
        let fkey = FlightKey(key.clone());
        let hash = key_hash_hex(&key);

        let (handle, role) = {
            use dashmap::mapref::entry::Entry;
            match self.in_flight.entry(fkey.clone()) {
                Entry::Occupied(e) => (e.get().clone(), "waiter"),
                Entry::Vacant(e) => {
                    let map = Arc::clone(&self.in_flight);
                    let fkey2 = fkey.clone();
                    let shared: Flight = async move {
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

        let span = tracing::debug_span!(
            "coalesce",
            flight.role = role,
            flight.key_hash = %hash,
            grpc.path = path,
        );
        handle.instrument(span).await
    }

    #[allow(dead_code)]
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.len()
    }
}

impl Default for Singleflight {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Singleflight {
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
        let sf = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let sf = sf.clone();
            let counter = counter.clone();
            handles.push(tokio::spawn(async move {
                sf.run(b"key".to_vec(), "/pkg.Svc/Method", async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok::<Bytes, String>(Bytes::from("result"))
                })
                .await
            }));
        }

        let results = futures_util::future::join_all(handles).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        for r in results {
            assert_eq!(r.unwrap().unwrap(), Bytes::from("result"));
        }
        assert_eq!(sf.in_flight_count(), 0);
    }

    #[tokio::test]
    async fn different_keys_are_not_coalesced() {
        let sf = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for i in 0u8..4 {
            let sf = sf.clone();
            let counter = counter.clone();
            let key = vec![i];
            handles.push(tokio::spawn(async move {
                sf.run(key, "/svc/m", async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok::<Bytes, String>(Bytes::from(vec![i]))
                })
                .await
            }));
        }

        futures_util::future::join_all(handles).await;
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn error_shared_but_not_cached() {
        let sf = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..3 {
            let sf = sf.clone();
            let counter = counter.clone();
            handles.push(tokio::spawn(async move {
                sf.run(b"k".to_vec(), "/s/m", async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Err::<Bytes, String>("boom".into())
                })
                .await
            }));
        }
        let results = futures_util::future::join_all(handles).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        for r in results {
            assert!(r.unwrap().is_err());
        }

        // After an error, the next call drives a fresh upstream attempt.
        let c2 = counter.clone();
        let result = sf
            .run(b"k".to_vec(), "/s/m", async move {
                c2.fetch_add(1, Ordering::SeqCst);
                Ok::<Bytes, String>(Bytes::from("ok"))
            })
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_eq!(result.unwrap(), Bytes::from("ok"));
    }
}
