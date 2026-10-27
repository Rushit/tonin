//! HTTP/2 listener and per-request dispatch pipeline.
//!
//! Each inbound gRPC request flows through:
//!   cache hit? → return cached bytes
//!   circuit breaker open? → return UNAVAILABLE
//!   singleflight.run {
//!       retry { upstream HTTP/2 via pooled connection }
//!   }
//!   record outcome → populate cache on success
//!
//! The proxy is gRPC-aware: it reads the `:path` pseudo-header (the full
//! method path, e.g. `/pkg.Service/Method`) and the raw request body so
//! coalescing and caching can key on (path, body). All other headers —
//! including `traceparent`, `tracestate`, `baggage`, `grpc-timeout` —
//! are forwarded verbatim to the upstream.
//!
//! Upstream connections are pooled (`TONIN_PROXY_POOL_SIZE`, 8 by default)
//! with HTTP/2 keep-alives (PING every 30s) so idle connections are not
//! pruned by Cilium/K8s and dead ones are detected and replaced; see
//! `pool.rs`.
//!
//! A non-OK `grpc-status` from the upstream is forwarded to the caller with
//! its original code and message. It is never cached, and only codes that
//! signal an unhealthy upstream (see `UpstreamError::is_upstream_fault`)
//! count against the circuit breaker.
//!
//! On SIGTERM/SIGINT the proxy keeps serving for `TONIN_PROXY_SHUTDOWN_DELAY_MS`
//! (the app container is shutting down at the same time and may still call
//! out), then stops accepting, sends GOAWAY, and waits for in-flight
//! requests to finish.
//!
//! ## gRPC framing and trailers
//!
//! gRPC requires `grpc-status` in HTTP/2 **trailing** HEADERS frames, not
//! initial headers. `Full<Bytes>` cannot emit trailers, so all response
//! helpers use `StreamBody` which yields `Frame::data` followed by
//! `Frame::trailers`, producing correct HTTP/2 framing:
//!
//!   HEADERS :status=200 content-type=application/grpc
//!   DATA     <5-byte-prefixed protobuf>
//!   HEADERS  grpc-status=0   ← END_STREAM trailing HEADERS

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use crate::breaker::CircuitBreakerMap;
use crate::cache::ResponseCache;
use crate::coalesce::{Singleflight, make_key};
use crate::config::Config;
use crate::pool::{ConnectionPool, PoolOptions};
use crate::retry::with_retry;
use crate::status::{UNKNOWN, UpstreamError};

type GrpcBody = BoxBody<Bytes, Infallible>;

/// Upper bound on TCP connect + HTTP/2 handshake to the upstream.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for in-flight requests after GOAWAY before exiting.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared proxy state cloned per connection.
#[derive(Clone)]
struct ProxyState {
    pool: Arc<ConnectionPool>,
    coalesce: Singleflight,
    /// Shared across clones; `ResponseCache` owns its map by value.
    cache: Option<Arc<ResponseCache>>,
    breaker: Arc<CircuitBreakerMap>,
    retry_max: u32,
}

impl ProxyState {
    fn new(cfg: &Config) -> anyhow::Result<Self> {
        let pool = ConnectionPool::new(
            &cfg.upstream,
            PoolOptions {
                size: cfg.pool_size,
                connect_timeout: CONNECT_TIMEOUT,
                keepalive_interval: cfg.keepalive_interval,
                keepalive_timeout: cfg.keepalive_timeout,
            },
        )?;
        Ok(Self {
            pool: Arc::new(pool),
            coalesce: Singleflight::new(),
            cache: if cfg.cache_enabled() {
                Some(Arc::new(ResponseCache::new(
                    cfg.cache_ttl,
                    cfg.cache_capacity,
                )))
            } else {
                None
            },
            breaker: Arc::new(CircuitBreakerMap::new(cfg.breaker_threshold)),
            retry_max: cfg.retry_max,
        })
    }
}

/// Start the proxy listener. Returns after a shutdown signal once in-flight
/// requests have drained.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let addr: SocketAddr = ([0, 0, 0, 0], cfg.port).into();
    let listener = TcpListener::bind(addr).await?;
    info!(port = cfg.port, upstream = %cfg.upstream, pool_size = cfg.pool_size, "tonin-proxy listening");

    let state = ProxyState::new(cfg)?;
    serve(
        listener,
        state,
        shutdown_signal(cfg.shutdown_delay),
        DRAIN_TIMEOUT,
    )
    .await
}

/// Accept connections until `shutdown` resolves, then drain.
async fn serve(
    listener: TcpListener,
    state: ProxyState,
    shutdown: impl Future<Output = ()>,
    drain_timeout: Duration,
) -> anyhow::Result<()> {
    let builder = AutoBuilder::new(TokioExecutor::new());
    let graceful = GracefulShutdown::new();
    tokio::pin!(shutdown);

    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => accepted?,
            () = &mut shutdown => break,
        };
        debug!(%peer, "accepted connection");
        let state = state.clone();
        let svc = hyper::service::service_fn(move |req| handle(state.clone(), req));
        let conn = graceful.watch(
            builder
                .serve_connection(TokioIo::new(stream), svc)
                .into_owned(),
        );
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                warn!(%peer, "connection error: {e}");
            }
        });
    }

    drop(listener);
    info!("tonin-proxy draining in-flight requests");
    tokio::select! {
        () = graceful.shutdown() => info!("tonin-proxy drained"),
        () = tokio::time::sleep(drain_timeout) => {
            warn!(?drain_timeout, "drain timed out; exiting with requests in flight");
        }
    }
    Ok(())
}

/// Resolve on SIGTERM or Ctrl-C, after `delay`. The proxy keeps serving
/// during the delay so the co-located app can finish its shutdown calls.
async fn shutdown_signal(delay: Duration) {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!("cannot listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                warn!("cannot listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    info!(?delay, "shutdown signal received; draining after delay");
    tokio::time::sleep(delay).await;
}

/// Handle a single HTTP/2 (gRPC) request.
async fn handle(
    state: ProxyState,
    req: Request<Incoming>,
) -> Result<Response<GrpcBody>, Infallible> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let headers = req.headers().clone();

    // Collect request body.
    let body_bytes = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            error!("failed to read request body: {e}");
            return Ok(grpc_error(&UpstreamError::new(
                UNKNOWN,
                "failed to read body",
            )));
        }
    };

    let cache_key = make_key(&path, &body_bytes);

    // --- Cache lookup ---
    if let Some(cache) = &state.cache
        && let Some(cached) = cache.get(&cache_key)
    {
        debug!(grpc.path = %path, "cache hit");
        return Ok(grpc_response(cached));
    }

    // --- Circuit breaker check ---
    if let Err(msg) = state.breaker.check(&path) {
        warn!(grpc.path = %path, "{msg}");
        return Ok(grpc_error(&UpstreamError::unavailable(msg)));
    }

    // --- Singleflight + retry + upstream ---
    let pool = Arc::clone(&state.pool);
    let retry_max = state.retry_max;
    let breaker = Arc::clone(&state.breaker);
    let path_clone = path.clone();
    let body_clone = body_bytes.clone();

    let result = state
        .coalesce
        .run(cache_key.clone(), &path, async move {
            with_retry(retry_max, &path_clone, || {
                let pool = Arc::clone(&pool);
                let path = path_clone.clone();
                let headers = headers.clone();
                let body = body_clone.clone();
                let method = method.clone();
                async move { pool.send_request(method, &path, &headers, body).await }
            })
            .await
            .inspect_err(|e| {
                if e.is_upstream_fault() {
                    breaker.record(&path_clone, false);
                }
            })
        })
        .await;

    match result {
        Ok(resp_bytes) => {
            state.breaker.record(&path, true);
            if let Some(cache) = &state.cache {
                cache.insert(&cache_key, resp_bytes.clone());
            }
            Ok(grpc_response(resp_bytes))
        }
        Err(e) if e.is_upstream_fault() => {
            error!(grpc.path = %path, "upstream error: {e}");
            Ok(grpc_error(&e))
        }
        Err(e) => {
            // An application-level answer (NOT_FOUND, INVALID_ARGUMENT, …):
            // the upstream is healthy, so it counts as a success for the breaker.
            debug!(grpc.path = %path, "upstream returned {e}");
            state.breaker.record(&path, true);
            Ok(grpc_error(&e))
        }
    }
}

/// Wrap raw gRPC body bytes in an HTTP/2 response with a trailing
/// `grpc-status: 0` HEADERS frame, as required by the gRPC spec.
fn grpc_response(body: Bytes) -> Response<GrpcBody> {
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", HeaderValue::from_static("0"));

    let frames = stream::iter([
        Ok::<Frame<Bytes>, Infallible>(Frame::data(body)),
        Ok(Frame::trailers(trailers)),
    ]);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .body(BoxBody::new(StreamBody::new(frames)))
        .unwrap()
}

/// Produce a gRPC trailers-only error response carrying `err`'s code,
/// message, and (when the upstream sent one) `grpc-status-details-bin`.
/// gRPC always uses HTTP 200 — the actual status goes in the
/// `grpc-status` trailer.
fn grpc_error(err: &UpstreamError) -> Response<GrpcBody> {
    let mut trailers = HeaderMap::new();
    trailers.insert("grpc-status", HeaderValue::from(err.code));
    if let Ok(v) = HeaderValue::from_str(&err.message) {
        trailers.insert("grpc-message", v);
    }
    if let Some(details) = &err.details {
        trailers.insert("grpc-status-details-bin", details.clone());
    }

    let frames = stream::iter([Ok::<Frame<Bytes>, Infallible>(Frame::trailers(trailers))]);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .body(BoxBody::new(StreamBody::new(frames)))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Upstream, UpstreamBehavior, grpc_call};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    struct Proxy {
        addr: SocketAddr,
        stop: Option<oneshot::Sender<()>>,
        task: JoinHandle<anyhow::Result<()>>,
    }

    async fn start_proxy(upstream: SocketAddr, cache_ttl: Duration) -> Proxy {
        let cfg = Config {
            port: 0,
            upstream: format!("http://{upstream}"),
            cache_ttl,
            cache_capacity: 100,
            retry_max: 3,
            breaker_threshold: 0.5,
            pool_size: 2,
            keepalive_interval: Some(Duration::from_millis(200)),
            keepalive_timeout: Duration::from_millis(200),
            shutdown_delay: Duration::ZERO,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let state = ProxyState::new(&cfg).unwrap();
        let task = tokio::spawn(serve(
            listener,
            state,
            async {
                let _ = stopped.await;
            },
            Duration::from_secs(2),
        ));
        Proxy {
            addr,
            stop: Some(stop),
            task,
        }
    }

    #[tokio::test]
    async fn upstream_grpc_error_is_forwarded_and_not_cached() {
        let up = Upstream::start(UpstreamBehavior::status(5, "user%20not%20found")).await;
        let proxy = start_proxy(up.addr, Duration::from_secs(60)).await;

        for _ in 0..2 {
            let reply = grpc_call(proxy.addr, "/pkg.Users/Get").await.unwrap();
            assert_eq!(reply.status.as_deref(), Some("5"));
            assert_eq!(reply.message.as_deref(), Some("user%20not%20found"));
            assert!(reply.body.is_empty());
        }
        assert_eq!(up.requests(), 2, "errors must not be served from cache");
    }

    #[tokio::test]
    async fn success_is_cached() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let proxy = start_proxy(up.addr, Duration::from_secs(60)).await;

        for _ in 0..2 {
            let reply = grpc_call(proxy.addr, "/pkg.Users/Get").await.unwrap();
            assert_eq!(reply.status.as_deref(), Some("0"));
            assert_eq!(reply.body, Bytes::from_static(b"hello"));
        }
        assert_eq!(up.requests(), 1);
    }

    #[tokio::test]
    async fn application_errors_do_not_open_breaker() {
        let up = Upstream::start(UpstreamBehavior::status(5, "missing")).await;
        let proxy = start_proxy(up.addr, Duration::ZERO).await;

        for _ in 0..25 {
            let reply = grpc_call(proxy.addr, "/pkg.Users/Get").await.unwrap();
            assert_eq!(reply.status.as_deref(), Some("5"));
        }
        assert_eq!(up.requests(), 25);
    }

    #[tokio::test]
    async fn upstream_faults_open_breaker() {
        // INTERNAL is a fault but not retryable: one upstream call per request.
        let up = Upstream::start(UpstreamBehavior::status(13, "boom")).await;
        let proxy = start_proxy(up.addr, Duration::ZERO).await;

        let mut last = None;
        for _ in 0..15 {
            last = Some(grpc_call(proxy.addr, "/pkg.Users/Get").await.unwrap());
        }
        let last = last.unwrap();
        assert_eq!(last.status.as_deref(), Some("14"));
        assert!(
            last.message
                .unwrap_or_default()
                .contains("circuit breaker open"),
            "breaker should be open"
        );
        assert!(up.requests() < 15, "open breaker must fail fast");
    }

    #[tokio::test]
    async fn retries_onto_fresh_connection_after_drop() {
        let up = Upstream::start(UpstreamBehavior::ok()).await;
        let proxy = start_proxy(up.addr, Duration::ZERO).await;
        grpc_call(proxy.addr, "/pkg.Users/Get").await.unwrap();
        grpc_call(proxy.addr, "/pkg.Users/Get").await.unwrap();

        // No pause: the next request may land on a connection the pool has
        // not noticed is dead yet; retry must carry it onto a new one.
        up.drop_connections();
        for _ in 0..4 {
            let reply = grpc_call(proxy.addr, "/pkg.Users/Get").await.unwrap();
            assert_eq!(reply.status.as_deref(), Some("0"));
        }
    }

    #[tokio::test]
    async fn shutdown_drains_in_flight_and_stops_accepting() {
        let up = Upstream::start(UpstreamBehavior::ok().delay(Duration::from_millis(300))).await;
        let mut proxy = start_proxy(up.addr, Duration::ZERO).await;
        let addr = proxy.addr;

        let in_flight = tokio::spawn(async move { grpc_call(addr, "/pkg.Users/Slow").await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        proxy.stop.take().unwrap().send(()).unwrap();

        let reply = in_flight.await.unwrap().unwrap();
        assert_eq!(
            reply.status.as_deref(),
            Some("0"),
            "in-flight request completes"
        );

        tokio::time::timeout(Duration::from_secs(2), &mut proxy.task)
            .await
            .expect("serve returns after drain")
            .unwrap()
            .unwrap();
        assert!(
            grpc_call(addr, "/pkg.Users/Get").await.is_err(),
            "no new connections after shutdown"
        );
    }
}
