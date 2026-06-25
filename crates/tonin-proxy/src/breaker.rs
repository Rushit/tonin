//! Per-path circuit breaker with Closed → Open → HalfOpen state machine.
//!
//! State is tracked in a `DashMap<path, BreakerState>` so each gRPC method
//! has an independent breaker. A global `threshold` ratio (from `Config`)
//! is the only tunable; window sizing uses a fixed sliding counter.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Open,
    HalfOpen,
}

struct BreakerInner {
    /// Total calls in the window.
    total: AtomicU64,
    /// Failed calls in the window.
    failures: AtomicU64,
    /// When the breaker tripped (for reset timeout).
    tripped_at: std::sync::Mutex<Option<Instant>>,
}

impl BreakerInner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            total: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            tripped_at: std::sync::Mutex::new(None),
        })
    }
}

pub struct CircuitBreakerMap {
    map: DashMap<String, Arc<BreakerInner>>,
    threshold: f64,
    min_requests: u64,
    reset_after: Duration,
}

impl CircuitBreakerMap {
    pub fn new(threshold: f64) -> Self {
        Self {
            map: DashMap::new(),
            threshold,
            min_requests: 10,
            reset_after: Duration::from_secs(30),
        }
    }

    fn get_or_create(&self, path: &str) -> Arc<BreakerInner> {
        if let Some(b) = self.map.get(path) {
            return Arc::clone(&b);
        }
        let b = BreakerInner::new();
        self.map.insert(path.to_string(), Arc::clone(&b));
        b
    }

    /// Check whether a call should proceed. Returns `Err` if the breaker is Open.
    pub fn check(&self, path: &str) -> Result<(), String> {
        let b = self.get_or_create(path);
        if self.state(&b) == State::Open {
            return Err(format!("circuit breaker open for {path}"));
        }
        Ok(())
    }

    /// Record the outcome of a completed call.
    pub fn record(&self, path: &str, success: bool) {
        let b = self.get_or_create(path);
        b.total.fetch_add(1, Ordering::Relaxed);
        if !success {
            b.failures.fetch_add(1, Ordering::Relaxed);
        }

        // Check whether to trip.
        let total = b.total.load(Ordering::Relaxed);
        let failures = b.failures.load(Ordering::Relaxed);
        if total >= self.min_requests {
            let rate = failures as f64 / total as f64;
            if rate >= self.threshold {
                let mut tripped = b.tripped_at.lock().unwrap();
                if tripped.is_none() {
                    *tripped = Some(Instant::now());
                }
            }
        }
    }

    fn state(&self, b: &BreakerInner) -> State {
        let tripped = b.tripped_at.lock().unwrap();
        match *tripped {
            None => State::Closed,
            Some(t) => {
                if Instant::now().duration_since(t) > self.reset_after {
                    State::HalfOpen
                } else {
                    State::Open
                }
            }
        }
    }

    /// Transition a HalfOpen breaker back to Closed on probe success.
    #[allow(dead_code)]
    pub fn reset(&self, path: &str) {
        if let Some(b) = self.map.get(path) {
            let mut tripped = b.tripped_at.lock().unwrap();
            *tripped = None;
            b.total.store(0, Ordering::Relaxed);
            b.failures.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_when_error_rate_exceeds_threshold() {
        let cb = CircuitBreakerMap::new(0.5);
        // 5 failures out of 10 = 0.5, should trip at the 10th call.
        for _ in 0..5 {
            cb.record("/svc/M", true);
        }
        for _ in 0..4 {
            cb.record("/svc/M", false);
        }
        // Still under min_requests threshold (9 total), breaker closed.
        assert_eq!(cb.check("/svc/M"), Ok(()));
        cb.record("/svc/M", false); // 10th call: 5/10 = 0.5, trips
        assert!(cb.check("/svc/M").is_err());
    }

    #[test]
    fn reset_clears_breaker() {
        let cb = CircuitBreakerMap::new(0.5);
        for _ in 0..10 {
            cb.record("/svc/M", false);
        }
        assert!(cb.check("/svc/M").is_err());
        cb.reset("/svc/M");
        assert_eq!(cb.check("/svc/M"), Ok(()));
    }

    #[test]
    fn unrelated_paths_are_independent() {
        let cb = CircuitBreakerMap::new(0.5);
        for _ in 0..10 {
            cb.record("/svc/A", false);
        }
        assert!(cb.check("/svc/A").is_err());
        assert_eq!(cb.check("/svc/B"), Ok(())); // B unaffected
    }
}
