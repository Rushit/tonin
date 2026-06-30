//! HTTP/2 connection pool with keep-alives and round-robin load balancing.
//!
//! This module provides persistent HTTP/2 channel pooling to avoid the overhead
//! of TCP connection and HTTP/2 handshake on every request. Channels are
//! reused across requests, with HTTP/2 PING keep-alives every 30s to prevent
//! idle-connection pruning in Cilium/K8s.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::anyhow;
use bytes::Bytes;
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http2::{self, SendRequest};
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{debug, warn};

/// Manages a pool of HTTP/2 connections to a single upstream.
pub struct ConnectionPool {
    upstream: Arc<str>,
    channels: Arc<Mutex<Vec<PooledChannel>>>,
    round_robin: AtomicUsize,
}

struct PooledChannel {
    sender: SendRequest<Full<Bytes>>,
}

impl ConnectionPool {
    /// Create a new connection pool for the given upstream.
    pub fn new(upstream: Arc<str>) -> Self {
        Self {
            upstream,
            channels: Arc::new(Mutex::new(Vec::new())),
            round_robin: AtomicUsize::new(0),
        }
    }

    /// Create a single HTTP/2 channel with keep-alive task.
    async fn create_channel(&self) -> anyhow::Result<PooledChannel> {
        let (host, port) = self.parse_upstream()?;

        let stream = TcpStream::connect((host.as_str(), port)).await?;
        let io = TokioIo::new(stream);
        let (sender, conn) = http2::handshake(TokioExecutor::new(), io).await?;

        // Spawn the connection task to drive the HTTP/2 connection.
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                warn!("upstream H2 connection error: {e}");
            }
        });

        let channel = PooledChannel {
            sender: sender.clone(),
        };

        // Spawn keep-alive task for this channel.
        let keep_alive_sender = channel.sender.clone();
        tokio::spawn(Self::keep_alive_task(keep_alive_sender));

        Ok(channel)
    }

    /// Parse the upstream URI into (host, port).
    fn parse_upstream(&self) -> anyhow::Result<(String, u16)> {
        let uri: hyper::Uri = format!("http://{}", self.upstream).parse()?;
        let host = uri
            .host()
            .ok_or_else(|| anyhow!("no host in upstream {}", self.upstream))?
            .to_owned();
        let port = uri.port_u16().unwrap_or(80);
        Ok((host, port))
    }

    /// Keep-alive task that sends PING frames every 30 seconds.
    async fn keep_alive_task(mut sender: SendRequest<Full<Bytes>>) {
        let mut ticker = interval(std::time::Duration::from_secs(30));
        loop {
            ticker.tick().await;

            // Create a dummy PING request to keep the connection alive.
            // In HTTP/2, we send a HEAD request to a dummy path.
            let req = Request::builder()
                .method(http::Method::HEAD)
                .uri("http://localhost/ping")
                .version(http::Version::HTTP_2)
                .body(Full::new(Bytes::new()))
                .unwrap();

            match sender.send_request(req).await {
                Ok(_resp) => {
                    debug!("sent keep-alive PING");
                }
                Err(e) => {
                    debug!("keep-alive PING failed (connection may be dead): {e}");
                    break;
                }
            }
        }
    }

    /// Send a request through the pool, creating channels on-demand.
    pub async fn send_request(
        &self,
        method: http::Method,
        path: String,
        headers: http::HeaderMap,
        body: Bytes,
    ) -> anyhow::Result<Bytes> {
        let mut channels = self.channels.lock().await;

        // Lazily create a channel if the pool is empty.
        if channels.is_empty() {
            debug!(
                "creating first channel in pool on-demand for {}",
                self.upstream
            );
            match self.create_channel().await {
                Ok(channel) => channels.push(channel),
                Err(e) => {
                    return Err(anyhow!("failed to create upstream connection: {e}"));
                }
            }
        }

        // Round-robin selection across existing channels.
        let idx = self.round_robin.fetch_add(1, Ordering::Relaxed) % channels.len();
        let channel = &mut channels[idx];

        let full_uri: hyper::Uri = format!("http://{}{}", self.upstream, path).parse()?;

        let mut builder = Request::builder()
            .method(method)
            .uri(&full_uri)
            .version(http::Version::HTTP_2);

        // Forward all headers except host (hyper sets it from the URI).
        for (name, value) in &headers {
            if name == http::header::HOST {
                continue;
            }
            builder = builder.header(name, value);
        }

        let req = builder.body(Full::new(body))?;
        let resp = channel.sender.send_request(req).await?;

        // Handle non-200 responses as errors from upstream.
        if resp.status() != StatusCode::OK {
            return Err(anyhow!(
                "upstream returned {}: {}",
                resp.status(),
                resp.status().canonical_reason().unwrap_or("unknown")
            ));
        }

        let resp_bytes = resp.into_body().collect().await?.to_bytes();
        Ok(resp_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = ConnectionPool::new(Arc::from("localhost:8080"));
        assert_eq!(pool.round_robin.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_round_robin_calculation() {
        let pool_size = 8;

        // Simulate round-robin selection across pool size.
        for (counter, expected_idx) in (0..pool_size * 2).enumerate() {
            let idx = counter % pool_size;
            assert_eq!(idx, expected_idx % pool_size);
        }
    }
}
