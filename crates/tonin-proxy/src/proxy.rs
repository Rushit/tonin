//! HTTP/2 listener and per-request dispatch pipeline.
//!
//! Each inbound gRPC request flows through:
//!   cache hit? → return cached bytes
//!   circuit breaker open? → return 503
//!   singleflight.run {
//!       retry { upstream HTTP/2 forward }
//!   }
//!   record outcome → populate cache on success
//!
//! The proxy is gRPC-aware: it reads the `:path` pseudo-header (the full
//! method path, e.g. `/pkg.Service/Method`) and the raw request body so
//! coalescing and caching can key on (path, body). All other headers —
//! including `traceparent`, `tracestate`, `baggage`, `grpc-timeout` —
//! are forwarded verbatim to the upstream.
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
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::stream;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use tokio::net::TcpListener;
use tracing::{debug, error, info, instrument, warn};

use crate::breaker::CircuitBreakerMap;
use crate::cache::ResponseCache;
use crate::coalesce::{Singleflight, make_key};
use crate::config::Config;
use crate::retry::with_retry;

type GrpcBody = BoxBody<Bytes, Infallible>;

/// Shared proxy state cloned per connection.
#[derive(Clone)]
struct ProxyState {
    upstream: Arc<str>,
    coalesce: Singleflight,
    cache: Option<ResponseCache>,
    breaker: Arc<CircuitBreakerMap>,
    retry_max: u32,
}

/// Start the proxy listener. Blocks until the process exits.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let addr: SocketAddr = ([0, 0, 0, 0], cfg.port).into();
    let listener = TcpListener::bind(addr).await?;
    info!(port = cfg.port, upstream = %cfg.upstream, "tonin-proxy listening");

    let state = ProxyState {
        upstream: Arc::from(cfg.upstream.as_str()),
        coalesce: Singleflight::new(),
        cache: if cfg.cache_enabled() {
            Some(ResponseCache::new(cfg.cache_ttl, cfg.cache_capacity))
        } else {
            None
        },
        breaker: Arc::new(CircuitBreakerMap::new(cfg.breaker_threshold)),
        retry_max: cfg.retry_max,
    };

    loop {
        let (stream, peer) = listener.accept().await?;
        debug!(%peer, "accepted connection");
        let state = state.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |req| {
                let state = state.clone();
                handle(state, req)
            });
            let io = TokioIo::new(stream);
            if let Err(e) = AutoBuilder::new(TokioExecutor::new())
                .serve_connection(io, svc)
                .await
            {
                warn!(%peer, "connection error: {e}");
            }
        });
    }
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
            return Ok(grpc_error("2", "failed to read body")); // UNKNOWN
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
        return Ok(grpc_error("14", &msg)); // UNAVAILABLE
    }

    // --- Singleflight + retry + upstream ---
    let upstream = Arc::clone(&state.upstream);
    let retry_max = state.retry_max;
    let breaker = Arc::clone(&state.breaker);
    let path_clone = path.clone();
    let headers_clone = headers.clone();
    let body_clone = body_bytes.clone();

    let result = state
        .coalesce
        .run(cache_key.clone(), &path, async move {
            with_retry(retry_max, &path_clone, || {
                let upstream = Arc::clone(&upstream);
                let path = path_clone.clone();
                let headers = headers_clone.clone();
                let body = body_clone.clone();
                let method = method.clone();
                async move {
                    forward(&upstream, method, path, headers, body)
                        .await
                        .map_err(|e| e.to_string())
                }
            })
            .await
            .inspect_err(|_| {
                breaker.record(&path_clone, false);
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
        Err(e) => {
            error!(grpc.path = %path, "upstream error: {e}");
            Ok(grpc_error("14", &e)) // UNAVAILABLE
        }
    }
}

/// Forward a single HTTP/2 request to the upstream and return the raw
/// response body bytes (the full gRPC response frame, including the
/// 5-byte length-prefix).
///
/// Uses `hyper::client::conn::http2::handshake` for explicit HTTP/2
/// prior-knowledge (RFC 7540 §3.4) rather than the legacy client.  The
/// legacy client with `http2_only(true)` can silently fall back to
/// HTTP/1.1 on plain `http://` URIs; the direct handshake is unambiguous.
#[instrument(skip_all, fields(grpc.path = %path))]
async fn forward(
    upstream: &str,
    method: http::Method,
    path: String,
    headers: http::HeaderMap,
    body: Bytes,
) -> anyhow::Result<Bytes> {
    use http_body_util::Full;
    use hyper::client::conn::http2;
    use tokio::net::TcpStream;

    let full_uri: hyper::Uri = format!("{upstream}{path}").parse()?;

    let host = full_uri
        .host()
        .ok_or_else(|| anyhow::anyhow!("no host in upstream URI {upstream}"))?
        .to_owned();
    let port = full_uri.port_u16().unwrap_or(80);

    // Open a plain TCP connection, then do HTTP/2 prior-knowledge handshake.
    let stream = TcpStream::connect((host.as_str(), port)).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = http2::handshake(TokioExecutor::new(), io).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            warn!("upstream H2 connection error: {e}");
        }
    });

    let mut builder = Request::builder()
        .method(method)
        .uri(full_uri)
        .version(http::Version::HTTP_2);

    // Forward all headers except host (hyper sets it from the URI).
    for (name, value) in &headers {
        if name == http::header::HOST {
            continue;
        }
        builder = builder.header(name, value);
    }

    let req = builder.body(Full::new(body))?;
    let resp = sender.send_request(req).await?;
    let resp_bytes = resp.into_body().collect().await?.to_bytes();
    Ok(resp_bytes)
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

/// Produce a gRPC trailers-only error response.
///
/// `grpc_code` is the numeric gRPC status code string (e.g. `"14"` for
/// UNAVAILABLE). gRPC always uses HTTP 200 — the actual status goes in the
/// `grpc-status` trailer.
fn grpc_error(grpc_code: &str, msg: &str) -> Response<GrpcBody> {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "grpc-status",
        HeaderValue::from_str(grpc_code).unwrap_or(HeaderValue::from_static("2")),
    );
    if let Ok(v) = HeaderValue::from_str(msg) {
        trailers.insert("grpc-message", v);
    }

    let frames = stream::iter([Ok::<Frame<Bytes>, Infallible>(Frame::trailers(trailers))]);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .body(BoxBody::new(StreamBody::new(frames)))
        .unwrap()
}
