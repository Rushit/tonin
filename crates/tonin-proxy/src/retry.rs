//! Retry executor with exponential backoff for upstream gRPC calls.
//!
//! Retries only on gRPC status codes that are safe to retry (Unavailable
//! and DeadlineExceeded). Any other error code — including Internal — is
//! returned immediately to avoid duplicating side-effecting RPCs.

use std::future::Future;
use std::time::Duration;

use tracing::warn;

/// Which gRPC-style error strings warrant a retry.
/// We match the raw status code names that tonic embeds in its `Status`.
fn is_retryable(err: &str) -> bool {
    err.contains("Unavailable") || err.contains("DeadlineExceeded")
}

/// Attempt `f` up to `max_attempts` times with exponential backoff starting
/// at 50 ms (cap 2 s). Returns the last error if all attempts fail.
pub async fn with_retry<F, Fut>(
    max_attempts: u32,
    path: &str,
    mut f: F,
) -> Result<bytes::Bytes, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bytes::Bytes, String>>,
{
    let mut backoff_ms = 50u64;
    for attempt in 1..=max_attempts {
        match f().await {
            Ok(b) => return Ok(b),
            Err(e) => {
                if attempt == max_attempts || !is_retryable(&e) {
                    return Err(e);
                }
                warn!(
                    grpc.path = path,
                    attempt, max_attempts, "upstream error, retrying after {backoff_ms}ms: {e}"
                );
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(2_000);
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn succeeds_on_first_attempt() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let result = with_retry(3, "/s/m", || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<Bytes, String>(Bytes::from("ok"))
            }
        })
        .await;
        assert_eq!(result.unwrap(), Bytes::from("ok"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_on_unavailable_then_succeeds() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let result = with_retry(3, "/s/m", || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err::<Bytes, String>("status: Unavailable".into())
                } else {
                    Ok(Bytes::from("ok"))
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), Bytes::from("ok"));
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_non_retryable_errors() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let result = with_retry(3, "/s/m", || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, String>("status: PermissionDenied".into())
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exhausts_retries_and_returns_last_error() {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let result = with_retry(3, "/s/m", || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<Bytes, String>("status: Unavailable".into())
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
}
