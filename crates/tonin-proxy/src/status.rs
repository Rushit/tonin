//! gRPC status carried from the upstream (or synthesised locally) back to
//! the caller.
//!
//! Upstream gRPC errors arrive as HTTP 200 with `grpc-status` in the
//! trailers (or in the headers, for trailers-only responses). They are
//! surfaced as [`UpstreamError`] so the proxy can forward the original
//! code and message instead of reporting success, and so retry and the
//! circuit breaker can decide on the numeric code rather than error text.

use std::fmt;

use http::{HeaderMap, HeaderValue, StatusCode};

pub const UNKNOWN: u32 = 2;
pub const DEADLINE_EXCEEDED: u32 = 4;
pub const PERMISSION_DENIED: u32 = 7;
pub const UNIMPLEMENTED: u32 = 12;
pub const INTERNAL: u32 = 13;
pub const UNAVAILABLE: u32 = 14;
pub const DATA_LOSS: u32 = 15;
pub const UNAUTHENTICATED: u32 = 16;

/// A non-OK gRPC outcome for one upstream call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpstreamError {
    pub code: u32,
    /// Already in `grpc-message` wire form when it came from the upstream.
    pub message: String,
    /// Raw `grpc-status-details-bin` from the upstream, forwarded untouched.
    pub details: Option<HeaderValue>,
}

impl UpstreamError {
    pub fn new(code: u32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(UNAVAILABLE, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(INTERNAL, message)
    }

    /// Map a non-200 HTTP status to a gRPC code, per the gRPC HTTP/2 spec
    /// ("HTTP to gRPC Status Code Mapping").
    pub fn from_http_status(status: StatusCode) -> Self {
        let code = match status.as_u16() {
            400 => INTERNAL,
            401 => UNAUTHENTICATED,
            403 => PERMISSION_DENIED,
            404 => UNIMPLEMENTED,
            429 | 502 | 503 | 504 => UNAVAILABLE,
            _ => UNKNOWN,
        };
        Self::new(code, format!("upstream returned HTTP {status}"))
    }

    /// Read `grpc-status` / `grpc-message` / `grpc-status-details-bin` from
    /// a header block. Returns `None` when `grpc-status` is absent,
    /// `Some(Ok(()))` for status 0, and `Some(Err(_))` otherwise.
    pub fn from_grpc_headers(headers: &HeaderMap) -> Option<Result<(), Self>> {
        let raw = headers.get("grpc-status")?;
        let code = match raw.to_str().ok().and_then(|s| s.trim().parse::<u32>().ok()) {
            Some(0) => return Some(Ok(())),
            Some(c) => c,
            None => {
                return Some(Err(Self::new(
                    UNKNOWN,
                    format!("upstream sent invalid grpc-status {raw:?}"),
                )));
            }
        };
        let message = headers
            .get("grpc-message")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        Some(Err(Self {
            code,
            message,
            details: headers.get("grpc-status-details-bin").cloned(),
        }))
    }

    /// Retry only codes that are safe to retry (see `retry.rs`).
    pub fn is_retryable(&self) -> bool {
        matches!(self.code, UNAVAILABLE | DEADLINE_EXCEEDED)
    }

    /// Whether this outcome says the upstream itself is unhealthy, as opposed
    /// to an application-level answer such as NOT_FOUND or INVALID_ARGUMENT.
    /// Only these count as failures for the circuit breaker.
    pub fn is_upstream_fault(&self) -> bool {
        matches!(
            self.code,
            UNKNOWN | DEADLINE_EXCEEDED | INTERNAL | UNAVAILABLE | DATA_LOSS
        )
    }
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "grpc-status {}: {}", self.code, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_headers_ok_error_and_absent() {
        let mut h = HeaderMap::new();
        assert!(UpstreamError::from_grpc_headers(&h).is_none());

        h.insert("grpc-status", HeaderValue::from_static("0"));
        assert_eq!(UpstreamError::from_grpc_headers(&h), Some(Ok(())));

        h.insert("grpc-status", HeaderValue::from_static("5"));
        h.insert(
            "grpc-message",
            HeaderValue::from_static("no%20such%20thing"),
        );
        h.insert("grpc-status-details-bin", HeaderValue::from_static("AAAA"));
        let err = UpstreamError::from_grpc_headers(&h).unwrap().unwrap_err();
        assert_eq!(err.code, 5);
        assert_eq!(err.message, "no%20such%20thing");
        assert_eq!(err.details, Some(HeaderValue::from_static("AAAA")));
    }

    #[test]
    fn invalid_grpc_status_is_unknown() {
        let mut h = HeaderMap::new();
        h.insert("grpc-status", HeaderValue::from_static("abc"));
        let err = UpstreamError::from_grpc_headers(&h).unwrap().unwrap_err();
        assert_eq!(err.code, UNKNOWN);
    }

    #[test]
    fn http_status_mapping() {
        assert_eq!(
            UpstreamError::from_http_status(StatusCode::SERVICE_UNAVAILABLE).code,
            UNAVAILABLE
        );
        assert_eq!(
            UpstreamError::from_http_status(StatusCode::NOT_FOUND).code,
            UNIMPLEMENTED
        );
        assert_eq!(
            UpstreamError::from_http_status(StatusCode::IM_A_TEAPOT).code,
            UNKNOWN
        );
    }

    #[test]
    fn application_errors_are_not_faults_or_retryable() {
        let not_found = UpstreamError::new(5, "");
        assert!(!not_found.is_upstream_fault());
        assert!(!not_found.is_retryable());
        assert!(UpstreamError::unavailable("").is_upstream_fault());
        assert!(UpstreamError::unavailable("").is_retryable());
        assert!(UpstreamError::internal("").is_upstream_fault());
        assert!(!UpstreamError::internal("").is_retryable());
    }
}
