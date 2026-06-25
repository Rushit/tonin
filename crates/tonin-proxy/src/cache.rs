//! Bounded TTL response cache keyed on (path, body) bytes.
//!
//! Only `Ok` responses are stored. Errors always fall through to the
//! upstream. Stale entries are evicted lazily on read. A hard capacity cap
//! degrades inserts to no-ops rather than evicting hot entries.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;

#[derive(Clone, PartialEq, Eq)]
struct CacheKey(Vec<u8>);

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

#[derive(Clone)]
struct Entry {
    value: Bytes,
    expires_at: Instant,
}

#[derive(Clone)]
pub struct ResponseCache {
    map: DashMap<CacheKey, Entry>,
    ttl: Duration,
    capacity: usize,
}

impl ResponseCache {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            map: DashMap::new(),
            ttl,
            capacity,
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<Bytes> {
        let k = CacheKey(key.to_vec());
        if let Some(e) = self.map.get(&k)
            && e.expires_at > Instant::now()
        {
            return Some(e.value.clone());
        }
        self.map.remove(&k);
        None
    }

    pub fn insert(&self, key: &[u8], value: Bytes) {
        if self.map.len() >= self.capacity {
            return;
        }
        self.map.insert(
            CacheKey(key.to_vec()),
            Entry {
                value,
                expires_at: Instant::now() + self.ttl,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn hit_within_ttl() {
        let c = ResponseCache::new(Duration::from_secs(10), 100);
        c.insert(b"k", Bytes::from("v"));
        assert_eq!(c.get(b"k").unwrap(), Bytes::from("v"));
    }

    #[test]
    fn miss_after_expiry() {
        let c = ResponseCache::new(Duration::from_millis(1), 100);
        c.insert(b"k", Bytes::from("v"));
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.get(b"k").is_none());
    }

    #[test]
    fn capacity_cap_drops_new_inserts() {
        let c = ResponseCache::new(Duration::from_secs(10), 2);
        c.insert(b"a", Bytes::from("1"));
        c.insert(b"b", Bytes::from("2"));
        c.insert(b"c", Bytes::from("3")); // dropped
        assert!(c.get(b"a").is_some());
        assert!(c.get(b"b").is_some());
        assert!(c.get(b"c").is_none());
    }
}
