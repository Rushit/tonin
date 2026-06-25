//! Request-header helpers for outbound calls.
//!
//! W3C traceparent + W3C tracestate injection so the callee can join
//! the caller's span. The framework's auto-generated client wrappers
//! call these for every outbound RPC; hand-written code can call them
//! too.
//!
//! Also provides [`inject_deadline`] for propagating a shrinking deadline
//! across service hops via the `grpc-timeout` header.
//!
//! The actual span lookup (tracing → opentelemetry context) lives in
//! `tonin-telemetry` on the server side. This crate just owns the
//! header-string format so peers without the telemetry crate can still
//! propagate manually.

use std::time::{Duration, Instant};

use tonic::Request;

const TRACEPARENT: &str = "traceparent";
const TRACESTATE: &str = "tracestate";

/// Inject a pre-formatted W3C traceparent string. The format is:
/// `00-<trace-id>-<span-id>-<trace-flags>` per the W3C spec.
///
/// Returns `true` if the header was injected, `false` if the value
/// failed to parse (in which case the request is left untouched).
pub fn inject_traceparent<T>(req: &mut Request<T>, traceparent: &str) -> bool {
    match traceparent.parse() {
        Ok(v) => {
            req.metadata_mut().insert(TRACEPARENT, v);
            true
        }
        Err(_) => {
            tracing::warn!(traceparent, "malformed traceparent — not injected");
            false
        }
    }
}

/// Inject a W3C tracestate string (vendor-specific trace data).
pub fn inject_tracestate<T>(req: &mut Request<T>, tracestate: &str) -> bool {
    match tracestate.parse() {
        Ok(v) => {
            req.metadata_mut().insert(TRACESTATE, v);
            true
        }
        Err(_) => false,
    }
}

/// Inject a `grpc-timeout` header derived from a deadline instant.
///
/// Computes the remaining budget as `deadline - Instant::now()` and
/// encodes it in gRPC timeout format (`<value><unit>`, e.g. `"450m"` for
/// 450 milliseconds). Returns `false` — without injecting — when the
/// deadline has already passed, so callers can short-circuit.
///
/// gRPC timeout units (largest that fits without truncation):
/// `H` hours · `M` minutes · `S` seconds · `m` milliseconds · `u` microseconds · `n` nanoseconds
pub fn inject_deadline<T>(req: &mut Request<T>, deadline: Instant) -> bool {
    let now = Instant::now();
    if deadline <= now {
        return false;
    }
    let remaining = deadline - now;
    let header = format_grpc_timeout(remaining);
    if let Ok(v) = header.parse() {
        req.metadata_mut().insert("grpc-timeout", v);
        true
    } else {
        false
    }
}

/// Encode a `Duration` as a gRPC timeout header value.
///
/// Picks the largest unit that represents the duration without loss of
/// precision beyond whole units, capped at hours.
pub(crate) fn format_grpc_timeout(d: Duration) -> String {
    let nanos = d.as_nanos();
    if nanos == 0 {
        return "0n".into();
    }
    // Walk from coarsest to finest, emit the first unit that divides evenly.
    let hours = nanos / 3_600_000_000_000;
    if hours > 0 && nanos.is_multiple_of(3_600_000_000_000) {
        return format!("{hours}H");
    }
    let mins = nanos / 60_000_000_000;
    if mins > 0 && nanos.is_multiple_of(60_000_000_000) {
        return format!("{mins}M");
    }
    let secs = nanos / 1_000_000_000;
    if secs > 0 && nanos.is_multiple_of(1_000_000_000) {
        return format!("{secs}S");
    }
    let millis = nanos / 1_000_000;
    if millis > 0 && nanos.is_multiple_of(1_000_000) {
        return format!("{millis}m");
    }
    let micros = nanos / 1_000;
    if micros > 0 && nanos.is_multiple_of(1_000) {
        return format!("{micros}u");
    }
    format!("{nanos}n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_traceparent_writes_header() {
        let mut req = Request::new(());
        let ok = inject_traceparent(
            &mut req,
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        );
        assert!(ok);
        let v = req.metadata().get("traceparent").unwrap();
        assert!(v.to_str().unwrap().starts_with("00-"));
    }

    #[test]
    fn inject_malformed_traceparent_is_noop() {
        let mut req = Request::new(());
        // Newline isn't a legal header byte.
        let ok = inject_traceparent(&mut req, "bad\nvalue");
        assert!(!ok);
        assert!(req.metadata().get("traceparent").is_none());
    }

    #[test]
    fn format_grpc_timeout_picks_coarsest_lossless_unit() {
        assert_eq!(format_grpc_timeout(Duration::from_secs(3600)), "1H");
        assert_eq!(format_grpc_timeout(Duration::from_secs(120)), "2M");
        assert_eq!(format_grpc_timeout(Duration::from_secs(5)), "5S");
        assert_eq!(format_grpc_timeout(Duration::from_millis(450)), "450m");
        assert_eq!(format_grpc_timeout(Duration::from_micros(750)), "750u");
        assert_eq!(format_grpc_timeout(Duration::from_nanos(123)), "123n");
    }

    #[test]
    fn format_grpc_timeout_sub_unit_boundary_uses_finer_unit() {
        // 1500 ms is not a whole number of seconds, so falls to millis.
        assert_eq!(format_grpc_timeout(Duration::from_millis(1500)), "1500m");
    }

    #[test]
    fn inject_deadline_future_writes_grpc_timeout() {
        let mut req = Request::new(());
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(inject_deadline(&mut req, deadline));
        let header = req.metadata().get("grpc-timeout").unwrap();
        let s = header.to_str().unwrap();
        // Should be some positive value ending in a valid unit.
        assert!(s.ends_with(['H', 'M', 'S', 'm', 'u', 'n']));
    }

    #[test]
    fn inject_deadline_past_returns_false() {
        let mut req = Request::new(());
        let past = Instant::now() - Duration::from_secs(1);
        assert!(!inject_deadline(&mut req, past));
        assert!(req.metadata().get("grpc-timeout").is_none());
    }
}
