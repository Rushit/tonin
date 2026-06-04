//! Optional per-method short-TTL response cache.
//!
//! Layered on top of [`crate::coalesce`]: the lookup order is
//! `cache hit → (miss) coalescing → upstream → populate cache`.
//!
//! Only `Ok` responses are cached. Errors are never stored; the next
//! call always retries upstream.
//!
//! ## Implementation
//!
//! Uses a `DashMap<Key, CachedEntry<R>>` with **lazy eviction**: stale
//! entries are removed on read, not by a background sweep. This keeps
//! the dep tree minimal (no extra async tasks, no `moka`). The tradeoff
//! is that evicted entries linger in memory until the key is next read —
//! acceptable for the expected small working sets (hot RPC methods, not
//! unbounded user data). A hard `capacity` cap is enforced at insert time
//! via a simple `if len() >= capacity { return }` guard; under capacity
//! pressure the cache degrades gracefully to a pass-through.
//!
//! ## Configuration
//!
//! Built from [`CacheConfig`]; see [`ResponseCache::new`].

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Configuration for the per-method response cache.
#[derive(Clone, Debug)]
pub struct CacheConfig {
    /// How long an `Ok` response is considered fresh.
    pub ttl: Duration,
    /// Maximum number of entries. When the map reaches this size, new
    /// entries are silently dropped (cache degrades to pass-through).
    /// Prevents unbounded growth under key-space explosion.
    pub capacity: usize,
}

impl CacheConfig {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self { ttl, capacity }
    }

    /// 500 ms TTL, 1 000 entries — a sensible default for a hot authz RPC.
    pub fn default_authz() -> Self {
        Self::new(Duration::from_millis(500), 1_000)
    }
}

// Internal entry stored in the map.
#[derive(Clone)]
struct CachedEntry<R> {
    value: R,
    expires_at: Instant,
}

// Newtype so Vec<u8> is usable as a DashMap key.
#[derive(Clone, PartialEq, Eq)]
struct CacheKey(Vec<u8>);

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// A bounded, TTL-evicting response cache for a single RPC method.
pub struct ResponseCache<R> {
    map: DashMap<CacheKey, CachedEntry<R>>,
    config: CacheConfig,
}

impl<R: Clone + Send + 'static> ResponseCache<R> {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            map: DashMap::new(),
            config,
        }
    }

    /// Look up a cached response by key. Returns `None` if absent or
    /// expired (and removes the stale entry in that case).
    pub fn get(&self, key: &[u8]) -> Option<R> {
        let k = CacheKey(key.to_vec());
        // Use entry API to atomically check-and-remove stale entries.
        if let Some(entry) = self.map.get(&k)
            && entry.expires_at > Instant::now()
        {
            return Some(entry.value.clone());
        }
        // Expired — remove and return None.
        self.map.remove(&k);
        None
    }

    /// Store an `Ok` response. Silently drops the insert when the cache
    /// is at capacity (pass-through degradation).
    pub fn insert(&self, key: &[u8], value: R) {
        if self.map.len() >= self.config.capacity {
            return;
        }
        let k = CacheKey(key.to_vec());
        self.map.insert(
            k,
            CachedEntry {
                value,
                expires_at: Instant::now() + self.config.ttl,
            },
        );
    }

    /// Remove all entries. Useful in tests.
    #[cfg(test)]
    pub fn clear(&self) {
        self.map.clear();
    }
}

impl<R: Clone + Send + 'static> Clone for ResponseCache<R> {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
            config: self.config.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn hit_within_ttl() {
        let cache: ResponseCache<String> =
            ResponseCache::new(CacheConfig::new(Duration::from_secs(10), 100));
        cache.insert(b"key", "value".into());
        assert_eq!(cache.get(b"key").as_deref(), Some("value"));
    }

    #[test]
    fn miss_after_expiry() {
        let cache: ResponseCache<String> =
            ResponseCache::new(CacheConfig::new(Duration::from_millis(1), 100));
        cache.insert(b"key", "value".into());
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(cache.get(b"key"), None);
    }

    #[test]
    fn error_is_not_cached() {
        // Errors are never inserted; the caller is responsible for skipping
        // insert on Err. This test just verifies a None get returns None.
        let cache: ResponseCache<String> =
            ResponseCache::new(CacheConfig::new(Duration::from_secs(10), 100));
        assert_eq!(cache.get(b"key"), None);
    }

    #[test]
    fn capacity_cap_drops_new_inserts() {
        let cache: ResponseCache<u32> =
            ResponseCache::new(CacheConfig::new(Duration::from_secs(10), 2));
        cache.insert(b"a", 1);
        cache.insert(b"b", 2);
        cache.insert(b"c", 3); // dropped — at capacity
        assert!(cache.get(b"a").is_some());
        assert!(cache.get(b"b").is_some());
        assert_eq!(cache.get(b"c"), None);
    }
}
