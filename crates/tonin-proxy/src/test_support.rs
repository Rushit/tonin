//! Test-only network fixtures: a local HTTP/2 gRPC upstream with scripted
//! responses, a TCP relay that counts HTTP/2 PINGs and can blackhole
//! connections, and a minimal gRPC client.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Request, Response};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

#[derive(Clone, Copy)]
enum Reply {
    /// `hello` body, then trailers with this status/message.
    Status(u32, &'static str),
    /// grpc-status in the response headers, no body, no trailers.
    TrailersOnly(u32, &'static str),
    /// `hello` body and no trailers at all.
    NoTrailers,
}

/// What the scripted upstream answers to every request.
#[derive(Clone, Copy)]
pub struct UpstreamBehavior {
    delay: Duration,
    reply: Reply,
}

impl UpstreamBehavior {
    pub fn ok() -> Self {
        Self::status(0, "")
    }
    pub fn status(code: u32, message: &'static str) -> Self {
        Self {
            delay: Duration::ZERO,
            reply: Reply::Status(code, message),
        }
    }
    pub fn trailers_only(code: u32, message: &'static str) -> Self {
        Self {
            delay: Duration::ZERO,
            reply: Reply::TrailersOnly(code, message),
        }
    }
    pub fn no_trailers() -> Self {
        Self {
            delay: Duration::ZERO,
            reply: Reply::NoTrailers,
        }
    }
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
}

#[derive(Default)]
struct Stats {
    requests: AtomicUsize,
    connections: AtomicUsize,
    open: AtomicUsize,
}

/// A local HTTP/2 gRPC server. Dropping it closes the listener and every
/// connection, like an upstream process exiting.
pub struct Upstream {
    pub addr: SocketAddr,
    stats: Arc<Stats>,
    kill: broadcast::Sender<()>,
    accept: JoinHandle<()>,
}

impl Upstream {
    pub async fn start(behavior: UpstreamBehavior) -> Self {
        Self::start_on("127.0.0.1:0".parse().unwrap(), behavior).await
    }

    pub async fn start_on(addr: SocketAddr, behavior: UpstreamBehavior) -> Self {
        let listener = TcpListener::bind(addr).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stats = Arc::new(Stats::default());
        let (kill, _) = broadcast::channel(1);

        let accept = {
            let stats = stats.clone();
            let kill = kill.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    stats.connections.fetch_add(1, Ordering::SeqCst);
                    stats.open.fetch_add(1, Ordering::SeqCst);
                    let mut killed = kill.subscribe();
                    let stats = stats.clone();
                    tokio::spawn(async move {
                        let svc = {
                            let stats = stats.clone();
                            hyper::service::service_fn(move |_req: Request<Incoming>| {
                                stats.requests.fetch_add(1, Ordering::SeqCst);
                                respond(behavior)
                            })
                        };
                        let conn = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                            .serve_connection(TokioIo::new(stream), svc);
                        tokio::select! {
                            _ = conn => {}
                            // Dropping `conn` closes the socket without GOAWAY.
                            _ = killed.recv() => {}
                        }
                        stats.open.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            })
        };

        Self {
            addr,
            stats,
            kill,
            accept,
        }
    }

    /// Abruptly close every open connection; the listener stays up.
    pub fn drop_connections(&self) {
        let _ = self.kill.send(());
    }

    pub fn requests(&self) -> usize {
        self.stats.requests.load(Ordering::SeqCst)
    }

    /// Connections accepted so far.
    pub fn connections(&self) -> usize {
        self.stats.connections.load(Ordering::SeqCst)
    }

    /// Connections currently open.
    pub fn open_connections(&self) -> usize {
        self.stats.open.load(Ordering::SeqCst)
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.accept.abort();
        let _ = self.kill.send(());
    }
}

async fn respond(
    behavior: UpstreamBehavior,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    tokio::time::sleep(behavior.delay).await;
    let grpc_headers = |code: u32, message: &str| {
        let mut h = HeaderMap::new();
        h.insert("grpc-status", HeaderValue::from(code));
        if !message.is_empty() {
            h.insert("grpc-message", HeaderValue::from_str(message).unwrap());
        }
        h
    };
    let resp = Response::builder().header("content-type", "application/grpc");
    let resp = match behavior.reply {
        Reply::Status(code, message) => {
            let frames = futures_util::stream::iter([
                Ok(Frame::data(Bytes::from_static(b"hello"))),
                Ok(Frame::trailers(grpc_headers(code, message))),
            ]);
            resp.body(BoxBody::new(StreamBody::new(frames)))
        }
        Reply::TrailersOnly(code, message) => {
            let mut r = resp.body(BoxBody::new(Empty::new())).unwrap();
            r.headers_mut().extend(grpc_headers(code, message));
            return Ok(r);
        }
        Reply::NoTrailers => resp.body(BoxBody::new(Full::new(Bytes::from_static(b"hello")))),
    };
    Ok(resp.unwrap())
}

/// TCP relay between the pool and an upstream. Counts HTTP/2 PING frames
/// sent by the client, and can silently stop forwarding on existing
/// connections while new connections keep working.
pub struct Relay {
    pub addr: SocketAddr,
    pings: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    generation: Arc<AtomicUsize>,
    accept: JoinHandle<()>,
}

impl Relay {
    pub async fn start(backend: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pings = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicUsize::new(0));

        let accept = {
            let (pings, connections, generation) =
                (pings.clone(), connections.clone(), generation.clone());
            tokio::spawn(async move {
                loop {
                    let Ok((client, _)) = listener.accept().await else {
                        return;
                    };
                    connections.fetch_add(1, Ordering::SeqCst);
                    let born = generation.load(Ordering::SeqCst);
                    let server = TcpStream::connect(backend).await.unwrap();
                    let (cr, cw) = client.into_split();
                    let (sr, sw) = server.into_split();
                    let live = {
                        let generation = generation.clone();
                        move || generation.load(Ordering::SeqCst) == born
                    };
                    let mut scanner = PingScanner::default();
                    let pings = pings.clone();
                    tokio::spawn(pump(cr, sw, live.clone(), move |b| {
                        pings.fetch_add(scanner.feed(b), Ordering::SeqCst);
                    }));
                    tokio::spawn(pump(sr, cw, live, |_| {}));
                }
            })
        };

        Self {
            addr,
            pings,
            connections,
            generation,
            accept,
        }
    }

    /// Stop forwarding on all current connections without closing them.
    pub fn blackhole_existing(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn pings(&self) -> usize {
        self.pings.load(Ordering::SeqCst)
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    live: impl Fn() -> bool,
    mut inspect: impl FnMut(&[u8]),
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if !live() {
            continue; // blackholed: read and discard
        }
        inspect(&buf[..n]);
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

/// Counts non-ACK PING frames in a client-to-server HTTP/2 byte stream.
#[derive(Default)]
struct PingScanner {
    buf: Vec<u8>,
    preface_done: bool,
}

impl PingScanner {
    const PREFACE: usize = 24;

    fn feed(&mut self, bytes: &[u8]) -> usize {
        self.buf.extend_from_slice(bytes);
        if !self.preface_done {
            if self.buf.len() < Self::PREFACE {
                return 0;
            }
            self.buf.drain(..Self::PREFACE);
            self.preface_done = true;
        }
        let mut pings = 0;
        while self.buf.len() >= 9 {
            let len = u32::from_be_bytes([0, self.buf[0], self.buf[1], self.buf[2]]) as usize;
            if self.buf.len() < 9 + len {
                break;
            }
            let (kind, flags) = (self.buf[3], self.buf[4]);
            if kind == 0x6 && flags & 0x1 == 0 {
                pings += 1;
            }
            self.buf.drain(..9 + len);
        }
        pings
    }
}

/// Result of one gRPC call made with [`grpc_call`].
pub struct GrpcReply {
    pub status: Option<String>,
    pub message: Option<String>,
    pub body: Bytes,
}

/// Make one unary call to `addr` over a fresh HTTP/2 connection.
pub async fn grpc_call(addr: SocketAddr, path: &str) -> anyhow::Result<GrpcReply> {
    let stream = TcpStream::connect(addr).await?;
    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream)).await?;
    tokio::spawn(conn);
    let req = Request::post(format!("http://{addr}{path}"))
        .header("content-type", "application/grpc")
        .body(Full::new(Bytes::from_static(b"req")))?;
    let (parts, body) = sender.send_request(req).await?.into_parts();
    let collected = body.collect().await?;
    let trailers = collected.trailers().cloned().unwrap_or_default();
    let get = |k: &str| {
        trailers
            .get(k)
            .or_else(|| parts.headers.get(k))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    Ok(GrpcReply {
        status: get("grpc-status"),
        message: get("grpc-message"),
        body: collected.to_bytes(),
    })
}
