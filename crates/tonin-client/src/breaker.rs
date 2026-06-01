//! Circuit-breaker configuration for outbound calls.
//!
//! Like [`crate::retry`], the active mechanism (a tower::Layer that
//! tracks failure rates) ships with the framework — but the **config
//! type lives here** so peer services can tune the breaker without
//! depending on the framework crate.
//!
//! ## Typical use (peer service)
//!
//! ```ignore
//! use tonin_client::breaker::CircuitBreaker;
//! use greeter_client::GreeterClient;
//!
//! let client = GreeterClient::connect("http://greeter:50051").await?
//!     .with_circuit_breaker(CircuitBreaker::default());
//! ```
//!
//! ## State machine
//!
//! Standard three-state breaker:
//!
//! - **Closed**: traffic flows. Failures counted in a rolling window.
//! - **Open**: failure rate crossed [`CircuitBreaker::trip_threshold`]; calls
//!   short-circuit with `Status::unavailable` for [`CircuitBreaker::reset_after`].
//! - **HalfOpen**: a small number of probe requests get through. If
//!   any succeed, transition back to Closed; if any fail, return to Open.

use std::time::Duration;

#[derive(Clone, Debug)]
pub struct CircuitBreaker {
    /// Rolling-window size for failure-rate calculation.
    pub window: Duration,
    /// Failure ratio in `(0, 1]` at which the breaker trips.
    pub trip_threshold: f64,
    /// Minimum number of requests observed in the window before the
    /// breaker can trip. Prevents tripping off one isolated failure.
    pub min_requests: u32,
    /// How long the breaker stays Open before transitioning to HalfOpen.
    pub reset_after: Duration,
    /// Number of probe requests allowed through in HalfOpen state.
    pub half_open_probes: u32,
    /// Which gRPC codes count as failures. Defaults match retry's
    /// retryable set — Unavailable + DeadlineExceeded.
    pub failure_codes: Vec<tonic::Code>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(10),
            trip_threshold: 0.5,
            min_requests: 20,
            reset_after: Duration::from_secs(30),
            half_open_probes: 3,
            failure_codes: vec![tonic::Code::Unavailable, tonic::Code::DeadlineExceeded],
        }
    }
}

impl CircuitBreaker {
    /// Aggressive breaker — trips on first sustained slowness. Useful
    /// for non-critical-path RPCs where you'd rather fail fast than
    /// queue up.
    pub fn aggressive() -> Self {
        Self {
            window: Duration::from_secs(5),
            trip_threshold: 0.3,
            min_requests: 5,
            reset_after: Duration::from_secs(15),
            half_open_probes: 1,
            failure_codes: vec![tonic::Code::Unavailable, tonic::Code::DeadlineExceeded],
        }
    }

    /// Conservative breaker — only trips on widespread, sustained
    /// failure. For critical-path RPCs where you'd rather queue than
    /// shed.
    pub fn conservative() -> Self {
        Self {
            window: Duration::from_secs(30),
            trip_threshold: 0.75,
            min_requests: 50,
            reset_after: Duration::from_secs(60),
            half_open_probes: 5,
            failure_codes: vec![tonic::Code::Unavailable],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_trip_threshold_is_half() {
        let cb = CircuitBreaker::default();
        assert!((cb.trip_threshold - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn aggressive_trips_faster_than_conservative() {
        assert!(
            CircuitBreaker::aggressive().min_requests < CircuitBreaker::conservative().min_requests
        );
        assert!(
            CircuitBreaker::aggressive().trip_threshold
                < CircuitBreaker::conservative().trip_threshold
        );
    }
}
