//! Request-header helpers for outbound calls.
//!
//! W3C traceparent + W3C tracestate injection so the callee can join
//! the caller's span. The framework's auto-generated client wrappers
//! call these for every outbound RPC; hand-written code can call them
//! too.
//!
//! The actual span lookup (tracing → opentelemetry context) lives in
//! `tonin-telemetry` on the server side. This crate just owns the
//! header-string format so peers without the telemetry crate can still
//! propagate manually.

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
}
