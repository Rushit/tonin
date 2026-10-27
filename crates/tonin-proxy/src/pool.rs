//! HTTP/2 connection pool with keep-alives and round-robin load balancing.
//!
//! The pool holds up to `size` persistent HTTP/2 connections to the
//! upstream. Each request picks the next slot round-robin and multiplexes a
//! new stream onto that slot's connection; a cloned `SendRequest` is used,
//! so no lock is held while a request is in flight.
//!
//! Slots fill lazily. A slot whose connection has closed (upstream restart,
//! idle pruning by Cilium/conntrack, keep-alive timeout) is reconnected on
//! its next use. HTTP/2 PING keep-alives run on every connection, including
//! idle ones, so a silently dropped connection is detected and closed
//! instead of hanging the next request.
//!
//! Connection failures are reported as gRPC UNAVAILABLE, which `retry.rs`
//! retries — the retry lands on a freshly reconnected slot.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http2::{self, SendRequest};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::net::TcpStream;
use tracing::{debug, warn};

use crate::status::UpstreamError;

type Sender = SendRequest<Full<Bytes>>;

/// Pool tuning. Defaults come from `Config`.
#[derive(Clone, Debug)]
pub struct PoolOptions {
    /// Number of HTTP/2 connections to keep to the upstream (at least 1).
    pub size: usize,
    /// Upper bound on TCP connect + HTTP/2 handshake.
    pub connect_timeout: Duration,
    /// Interval between HTTP/2 PINGs. `None` disables keep-alives.
    pub keepalive_interval: Option<Duration>,
    /// How long to wait for a PING ack before closing the connection.
    pub keepalive_timeout: Duration,
}

/// Manages a pool of HTTP/2 connections to a single upstream.
pub struct ConnectionPool {
    upstream: String,
    host: String,
    port: u16,
    opts: PoolOptions,
    slots: Box<[Mutex<Option<Sender>>]>,
    next: AtomicUsize,
}

impl ConnectionPool {
    /// Create a pool for `upstream` (`http://host:port`). No connection is
    /// opened until the first request.
    pub fn new(upstream: &str, opts: PoolOptions) -> anyhow::Result<Self> {
        let uri: hyper::Uri = upstream.parse()?;
        let host = uri
            .host()
            .ok_or_else(|| anyhow::anyhow!("no host in upstream {upstream}"))?
            .to_owned();
        let port = uri.port_u16().unwrap_or(80);
        let slots = (0..opts.size.max(1)).map(|_| Mutex::new(None)).collect();
        Ok(Self {
            upstream: upstream.trim_end_matches('/').to_owned(),
            host,
            port,
            opts,
            slots,
            next: AtomicUsize::new(0),
        })
    }

    /// Open one HTTP/2 connection with keep-alives enabled.
    async fn connect(&self) -> Result<Sender, UpstreamError> {
        let handshake = async {
            let stream = TcpStream::connect((self.host.as_str(), self.port)).await?;
            stream.set_nodelay(true)?;
            let mut builder = http2::Builder::new(TokioExecutor::new());
            builder.timer(TokioTimer::new());
            if let Some(interval) = self.opts.keepalive_interval {
                builder
                    .keep_alive_interval(interval)
                    .keep_alive_timeout(self.opts.keepalive_timeout)
                    .keep_alive_while_idle(true);
            }
            let (sender, conn) = builder.handshake(TokioIo::new(stream)).await?;
            anyhow::Ok((sender, conn))
        };

        let (sender, conn) = tokio::time::timeout(self.opts.connect_timeout, handshake)
            .await
            .map_err(|_| {
                UpstreamError::unavailable(format!(
                    "timed out connecting to upstream {}",
                    self.upstream
                ))
            })?
            .map_err(|e| {
                UpstreamError::unavailable(format!(
                    "failed to connect to upstream {}: {e}",
                    self.upstream
                ))
            })?;

        // Drives the connection; ends when the peer closes, a keep-alive
        // PING goes unacknowledged, or every `Sender` clone is dropped.
        let upstream = self.upstream.clone();
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                warn!(%upstream, "upstream H2 connection closed: {e}");
            }
        });
        debug!(upstream = %self.upstream, "opened upstream connection");
        Ok(sender)
    }

    /// Return a live sender for the next slot, reconnecting it if needed.
    async fn sender(&self) -> Result<Sender, UpstreamError> {
        let slot = &self.slots[self.next.fetch_add(1, Ordering::Relaxed) % self.slots.len()];
        if let Some(s) = lock(slot).as_ref().filter(|s| !s.is_closed()) {
            return Ok(s.clone());
        }

        // Connect without holding the slot lock. If another request filled
        // the slot meanwhile, use its connection and drop ours.
        let fresh = self.connect().await?;
        let mut guard = lock(slot);
        match guard.as_ref().filter(|s| !s.is_closed()) {
            Some(existing) => Ok(existing.clone()),
            None => {
                *guard = Some(fresh.clone());
                Ok(fresh)
            }
        }
    }

    /// Send one unary gRPC request and return the response body.
    ///
    /// A non-zero `grpc-status` from the upstream is returned as an
    /// [`UpstreamError`] carrying the upstream's code and message.
    pub async fn send_request(
        &self,
        method: http::Method,
        path: &str,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Bytes, UpstreamError> {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.upstream))
            .version(http::Version::HTTP_2);

        // Forward all headers except host (hyper sets it from the URI).
        for (name, value) in headers {
            if name == http::header::HOST {
                continue;
            }
            builder = builder.header(name, value);
        }
        let req = builder
            .body(Full::new(body))
            .map_err(|e| UpstreamError::internal(format!("invalid upstream request: {e}")))?;

        let mut sender = self.sender().await?;
        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| UpstreamError::unavailable(format!("upstream request failed: {e}")))?;

        let (parts, body) = resp.into_parts();
        if parts.status != StatusCode::OK {
            return Err(UpstreamError::from_http_status(parts.status));
        }
        // Trailers-only responses carry grpc-status in the headers.
        if let Some(status) = UpstreamError::from_grpc_headers(&parts.headers) {
            return status.map(|()| Bytes::new());
        }

        let collected = body.collect().await.map_err(|e| {
            UpstreamError::unavailable(format!("upstream response body failed: {e}"))
        })?;
        let status = collected
            .trailers()
            .and_then(UpstreamError::from_grpc_headers)
            .unwrap_or_else(|| {
                Err(UpstreamError::internal(
                    "upstream response ended without grpc-status",
                ))
            });
        status.map(|()| collected.to_bytes())
    }
}

/// Lock a slot. The guarded value is a plain `Option`, so a poisoned lock
/// is still consistent and safe to reuse.
fn lock(slot: &Mutex<Option<Sender>>) -> std::sync::MutexGuard<'_, Option<Sender>> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::UNAVAILABLE;
    use crate::test_support::{Relay, Upstream, UpstreamBehavior};
    use std::sync::Arc;
    use std::time::Instant;

    fn opts(size: usize) -> PoolOptions {
        PoolOptions {
            size,
            connect_timeout: Duration::from_secs(2),
            keepalive_interval: Some(Duration::from_millis(100)),
            keepalive_timeout: Duration::from_millis(100),
        }
    }

    fn pool(addr: std::net::SocketAddr, opts: PoolOptions) -> ConnectionPool {
        ConnectionPool::new(&format!("http://{addr}"), opts).unwrap()
    }

    async fn call(pool: &ConnectionPool) -> Result<Bytes, UpstreamError> {
        pool.send_request(
            http::Method::POST,
            "/pkg.Svc/Method",
            &HeaderMap::new(),
            Bytes::from_static(b"req"),
        )
        .await
    }

    #[test]
    fn rejects_upstream_without_host() {
        assert!(ConnectionPool::new("/just/a/path", opts(1)).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_requests_are_not_serialized() {
        let up = Upstream::start(UpstreamBehavior::ok().delay(Duration::from_millis(300))).await;
        let pool = Arc::new(pool(up.addr, opts(1)));

        let start = Instant::now();
        let calls: Vec<_> = (0..8)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move { call(&pool).await })
            })
            .collect();
        for c in calls {
            assert_eq!(c.await.unwrap().unwrap(), Bytes::from_static(b"hello"));
        }
        // Serialized, 8 × 300 ms would be ≥ 2.4 s.
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(1_000), "took {elapsed:?}");
        assert_eq!(up.requests(), 8);
    }

    #[tokio::test]
    async fn reuses_connections_up_to_pool_size() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let pool = pool(up.addr, opts(3));
        for _ in 0..30 {
            call(&pool).await.unwrap();
        }
        assert_eq!(up.requests(), 30);
        assert_eq!(up.connections(), 3, "one connection per slot, then reuse");
    }

    #[tokio::test]
    async fn reconnects_after_upstream_drops_connections() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let pool = pool(up.addr, opts(1));
        call(&pool).await.unwrap();

        up.drop_connections();
        tokio::time::sleep(Duration::from_millis(50)).await;

        for _ in 0..3 {
            call(&pool).await.unwrap();
        }
        assert_eq!(up.connections(), 2, "exactly one reconnect");
    }

    #[tokio::test]
    async fn recovers_after_upstream_restart() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let addr = up.addr;
        let pool = pool(addr, opts(1));
        call(&pool).await.unwrap();

        drop(up);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let err = call(&pool).await.unwrap_err();
        assert_eq!(err.code, UNAVAILABLE, "{err}");

        let _up = Upstream::start_on(addr, UpstreamBehavior::ok()).await;
        assert_eq!(call(&pool).await.unwrap(), Bytes::from_static(b"hello"));
    }

    #[tokio::test]
    async fn sends_keepalive_pings_while_idle() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let relay = Relay::start(up.addr).await;
        let pool = pool(relay.addr, opts(1));
        call(&pool).await.unwrap();

        tokio::time::sleep(Duration::from_millis(550)).await;
        let pings = relay.pings();
        assert!(
            pings >= 3,
            "expected ≥3 PINGs in 550 ms at 100 ms, got {pings}"
        );
    }

    #[tokio::test]
    async fn no_pings_when_keepalive_disabled() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let relay = Relay::start(up.addr).await;
        let pool = pool(
            relay.addr,
            PoolOptions {
                keepalive_interval: None,
                ..opts(1)
            },
        );
        call(&pool).await.unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(relay.pings(), 0);
    }

    #[tokio::test]
    async fn keepalive_detects_blackholed_connection() {
        // The relay stops forwarding on the existing connection without
        // closing it — what a pruned conntrack entry looks like to the
        // client. New connections through the relay still work.
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let relay = Relay::start(up.addr).await;
        let pool = pool(relay.addr, opts(1));
        call(&pool).await.unwrap();

        relay.blackhole_existing();
        // interval (100 ms) + ack timeout (100 ms), with slack.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let res = tokio::time::timeout(Duration::from_secs(2), call(&pool))
            .await
            .expect("request on a blackholed connection must not hang");
        assert_eq!(res.unwrap(), Bytes::from_static(b"hello"));
        assert_eq!(relay.connections(), 2);
    }

    #[tokio::test]
    async fn in_flight_request_on_blackholed_connection_fails_fast() {
        let up = Upstream::start(UpstreamBehavior::ok().delay(Duration::from_millis(200))).await;
        let relay = Relay::start(up.addr).await;
        let pool = Arc::new(pool(relay.addr, opts(1)));
        call(&pool).await.unwrap();

        let in_flight = {
            let pool = pool.clone();
            tokio::spawn(async move { call(&pool).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        relay.blackhole_existing();

        let res = tokio::time::timeout(Duration::from_secs(2), in_flight)
            .await
            .expect("keep-alive must fail the stuck request")
            .unwrap();
        assert_eq!(res.unwrap_err().code, UNAVAILABLE);
    }

    #[tokio::test]
    async fn upstream_grpc_error_is_returned_not_masked() {
        let up = Upstream::start(UpstreamBehavior::status(5, "no%20such%20key")).await;
        let pool = pool(up.addr, opts(1));
        let err = call(&pool).await.unwrap_err();
        assert_eq!(err.code, 5);
        assert_eq!(err.message, "no%20such%20key");
    }

    #[tokio::test]
    async fn trailers_only_grpc_error_is_returned() {
        let up = Upstream::start(UpstreamBehavior::trailers_only(7, "denied")).await;
        let pool = pool(up.addr, opts(1));
        let err = call(&pool).await.unwrap_err();
        assert_eq!((err.code, err.message.as_str()), (7, "denied"));
    }

    #[tokio::test]
    async fn missing_grpc_status_is_internal() {
        let up = Upstream::start(UpstreamBehavior::no_trailers()).await;
        let pool = pool(up.addr, opts(1));
        assert_eq!(call(&pool).await.unwrap_err().code, crate::status::INTERNAL);
    }

    #[tokio::test]
    async fn dropping_pool_closes_upstream_connections() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let pool = pool(up.addr, opts(2));
        call(&pool).await.unwrap();
        call(&pool).await.unwrap();
        assert_eq!(up.open_connections(), 2);

        drop(pool);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(up.open_connections(), 0);
    }
}
