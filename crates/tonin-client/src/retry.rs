//! Retry-policy configuration for outbound calls.
//!
//! The actual retry mechanism (a tower::Layer that consumes a
//! `RetryPolicy`) ships with the server framework — but the **config
//! type lives here** so generated client SDKs can expose the knob to
//! peer services without dragging in the framework.
//!
//! ## Typical use (peer service)
//!
//! ```ignore
//! use tonin_client::retry::RetryPolicy;
//! use greeter_client::GreeterClient;
//!
//! let client = GreeterClient::connect("http://greeter:50051").await?
//!     .with_retry(RetryPolicy::exponential(3));
//! ```
//!
//! ## Defaults
//!
//! The default policy is **no retries**. Retries change observable
//! behavior (duplicated side effects, amplified load on a slow callee);
//! callers opt in explicitly.

use std::time::Duration;

/// Retry behavior for an outbound RPC.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Total attempts including the first. `1` = no retry; `3` = up to
    /// two retries.
    pub max_attempts: u32,
    /// Delay before the first retry. Subsequent delays follow [`Self::backoff`].
    pub initial_backoff: Duration,
    pub backoff: Backoff,
    /// Cap on any single backoff interval, regardless of multiplier.
    pub max_backoff: Duration,
    /// Which gRPC codes count as retryable. Anything outside this set
    /// bubbles up on first failure.
    pub retryable: RetryableCodes,
}

#[derive(Clone, Copy, Debug)]
pub enum Backoff {
    /// `initial_backoff * multiplier^attempt`. Standard exponential.
    Exponential { multiplier: f64 },
    /// Same delay between every retry.
    Fixed,
}

/// Which gRPC codes the retry layer should treat as retryable.
///
/// Default is "safe-only": `Unavailable` and `DeadlineExceeded`. Adding
/// `Internal` or `Aborted` to this set should be a deliberate call —
/// they often mean "the server thinks it processed your request".
#[derive(Clone, Debug)]
pub struct RetryableCodes(pub Vec<tonic::Code>);

impl Default for RetryableCodes {
    fn default() -> Self {
        Self(vec![
            tonic::Code::Unavailable,
            tonic::Code::DeadlineExceeded,
        ])
    }
}

impl RetryPolicy {
    /// No retries — single attempt, fail fast. Default.
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::from_millis(0),
            backoff: Backoff::Fixed,
            max_backoff: Duration::from_secs(1),
            retryable: RetryableCodes::default(),
        }
    }

    /// Exponential backoff with sane defaults: 50 ms → 100 ms → 200 ms.
    pub fn exponential(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            initial_backoff: Duration::from_millis(50),
            backoff: Backoff::Exponential { multiplier: 2.0 },
            max_backoff: Duration::from_secs(2),
            retryable: RetryableCodes::default(),
        }
    }

    /// Fixed delay between attempts.
    pub fn fixed(max_attempts: u32, delay: Duration) -> Self {
        Self {
            max_attempts,
            initial_backoff: delay,
            backoff: Backoff::Fixed,
            max_backoff: delay,
            retryable: RetryableCodes::default(),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_no_retry() {
        assert_eq!(RetryPolicy::default().max_attempts, 1);
    }

    #[test]
    fn exponential_defaults_have_reasonable_values() {
        let p = RetryPolicy::exponential(3);
        assert_eq!(p.max_attempts, 3);
        assert!(matches!(p.backoff, Backoff::Exponential { multiplier } if multiplier > 1.0));
    }
}
