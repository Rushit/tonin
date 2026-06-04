//! Demonstrates `CoalescingClient` against a real in-process gRPC server.
//!
//!   cargo run -p greeter --example coalesce_demo
//!
//! Shows:
//!   1. 8 concurrent identical requests → exactly 1 upstream call.
//!   2. Different names → each goes upstream independently.
//!   3. Error shared across all waiters, then retried fresh.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::future::join_all;
use prost::Message;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tonin_client::client::CoalescingClient;

pub mod proto {
    tonic::include_proto!("greeter.v1");
}

use proto::greeter_client::GreeterClient;
use proto::greeter_server::{Greeter, GreeterServer};
use proto::{HelloReply, HelloRequest};

// ── server ───────────────────────────────────────────────────────────────────

struct GreeterImpl {
    counter: Arc<AtomicU32>,
    delay_ms: u64,
    /// When Some(err), the next call returns that error (then clears).
    inject_error: Arc<std::sync::Mutex<Option<&'static str>>>,
}

#[tonic::async_trait]
impl Greeter for GreeterImpl {
    async fn say_hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        if let Some(msg) = self.inject_error.lock().unwrap().take() {
            return Err(Status::internal(msg));
        }
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        Ok(Response::new(HelloReply {
            message: format!("hello, {}", req.into_inner().name),
        }))
    }
}

async fn start_server(
    counter: Arc<AtomicU32>,
    delay_ms: u64,
    inject_error: Arc<std::sync::Mutex<Option<&'static str>>>,
) -> SocketAddr {
    // Bind inside an async context so tokio owns the socket from the start.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .add_service(GreeterServer::new(GreeterImpl {
                counter,
                delay_ms,
                inject_error,
            }))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

// ── helpers ──────────────────────────────────────────────────────────────────

type Client = CoalescingClient<GreeterClient<tonic::transport::Channel>>;

async fn connect(addr: SocketAddr) -> Arc<Client> {
    let inner = GreeterClient::connect(format!("http://{addr}"))
        .await
        .expect("connect");
    Arc::new(CoalescingClient::new(inner))
}

/// Make one coalesced SayHello call.
/// Pattern: clone `inner` BEFORE `call()` so the closure owns it, avoiding
/// a borrow conflict with `&self` in `call()`.
async fn say_hello(client: &Arc<Client>, name: &str) -> Result<HelloReply, Status> {
    let req = HelloRequest {
        name: name.to_string(),
    };
    let bytes = req.encode_to_vec();
    // Clone inner before calling — `say_hello` takes &mut self.
    let mut inner = client.inner.clone();
    client
        .call::<HelloReply, _, _>(
            "greeter.v1.Greeter",
            "SayHello",
            bytes,
            move || async move { inner.say_hello(Request::new(req)).await },
        )
        .await
        .map(Response::into_inner)
}

// ── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Test 1: coalescing ───────────────────────────────────────────────────

    println!("\n=== Test 1: coalescing ===");
    println!("  8 concurrent SayHello(\"world\") with 50 ms server delay.");

    let counter = Arc::new(AtomicU32::new(0));
    let no_err = Arc::new(std::sync::Mutex::new(None));
    let addr = start_server(counter.clone(), 50, no_err).await;
    let client = connect(addr).await;

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let c = Arc::clone(&client);
            tokio::spawn(async move { say_hello(&c, "world").await })
        })
        .collect();

    let results = join_all(handles).await;
    let upstream = counter.load(Ordering::SeqCst);
    println!("  upstream calls: {upstream}  (want 1)");
    assert_eq!(upstream, 1, "coalescing should reduce 8 calls to 1");

    for r in &results {
        assert_eq!(
            r.as_ref().unwrap().as_ref().unwrap().message,
            "hello, world"
        );
    }
    println!("  all 8 callers got \"hello, world\" ✓");

    // ── Test 2: different requests ───────────────────────────────────────────

    println!("\n=== Test 2: different names → different upstream calls ===");
    counter.store(0, Ordering::SeqCst);

    let handles: Vec<_> = ["alice", "bob", "carol", "dave"]
        .iter()
        .map(|name| {
            let c = Arc::clone(&client);
            let name = name.to_string();
            tokio::spawn(async move { say_hello(&c, &name).await })
        })
        .collect();

    join_all(handles).await;
    let upstream = counter.load(Ordering::SeqCst);
    println!("  upstream calls: {upstream}  (want 4)");
    assert_eq!(upstream, 4, "different requests must not coalesce");
    println!("  ✓");

    // ── Test 3: error shared, not cached — retry succeeds ───────────────────

    println!("\n=== Test 3: error shared across flight, not cached ===");

    let counter2 = Arc::new(AtomicU32::new(0));
    let err_flag = Arc::new(std::sync::Mutex::new(Some("injected failure")));
    let addr2 = start_server(counter2.clone(), 30, err_flag).await;
    let err_client = connect(addr2).await;

    // 3 concurrent calls — only the first goes upstream, returns an error.
    let handles: Vec<_> = (0..3)
        .map(|_| {
            let c = Arc::clone(&err_client);
            tokio::spawn(async move { say_hello(&c, "oops").await })
        })
        .collect();

    let results = join_all(handles).await;
    let upstream = counter2.load(Ordering::SeqCst);
    let errors = results
        .iter()
        .filter(|r| r.as_ref().unwrap().is_err())
        .count();

    println!("  upstream calls: {upstream}  (want 1)");
    println!("  callers that got the error: {errors}/3  (want 3)");
    assert_eq!(upstream, 1);
    assert_eq!(errors, 3, "all waiters should share the error");

    // Error was not cached — retry immediately should succeed.
    let retry = say_hello(&err_client, "oops").await;
    println!(
        "  retry result: {}  (want Ok)",
        if retry.is_ok() { "Ok ✓" } else { "Err ✗" }
    );
    assert!(retry.is_ok(), "retry after error should succeed");
    println!(
        "  upstream calls after retry: {}  (want 2)",
        counter2.load(Ordering::SeqCst)
    );
    assert_eq!(counter2.load(Ordering::SeqCst), 2);

    println!("\n=== All tests passed ✓ ===\n");
    Ok(())
}
