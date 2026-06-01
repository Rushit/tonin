# Building a gRPC service

The primary developer flow: one builder call wires a tonic gRPC server with telemetry, optional auth, and an optional MCP sidecar.

## What you get

From a single `Service::new(name).handler(svc).run()` chain you get:

- A tonic gRPC server bound to `0.0.0.0:50051` (overridable).
- W3C TraceContext extraction on every inbound RPC — incoming `traceparent` / `tracestate` headers become the parent span automatically.
- Idempotent OTLP telemetry init at construction time. Safe to call repeatedly in tests; honors `TONIN_TELEMETRY=off`.
- Optional JWT (or custom) auth via `.with_auth(...)`. The verified `AuthCtx` lands in both request extensions and a task-local readable inside handlers.
- Optional anonymous mode via `.without_auth()` — handlers still see an `AuthCtx { kind: Anonymous }`.
- Optional in-process MCP sidecar on a second port via `.enable_mcp()` / `.enable_mcp_with(...)`. Same process, same `AuthCtx`, shared lifecycle.
- Graceful telemetry flush on shutdown so the last spans reach the collector.

You do not write a `tonic::transport::Server::builder()` chain, install layers manually, or wire a tracing subscriber. The builder does it.

## Minimum viable service

The canonical sample is [`examples/greeter`](https://github.com/Rushit/tonin/tree/main/examples/greeter). Four files.

### `proto/greeter.proto`

```proto
syntax = "proto3";
package greeter.v1;

service Greeter {
  rpc SayHello (HelloRequest) returns (HelloReply);
}

message HelloRequest { string name = 1; }
message HelloReply   { string message = 1; }
```

### `build.rs`

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Skip codegen if protoc is unavailable so the workspace still `cargo check`s
    // without system protoc installed. Set TONIN_SKIP_PROTOC=1 to skip.
    if std::env::var("TONIN_SKIP_PROTOC").is_ok() {
        return Ok(());
    }
    tonin_build::compile(&["proto/greeter.proto"], &["proto"])
}
```

`tonin_build::compile` is a thin wrapper around `tonic-build` with the framework's conventions baked in. Generated code lands in `OUT_DIR`; include it from your service module with `tonic::include_proto!("greeter.v1")`.

### `src/main.rs`

```rust
use tonin::prelude::*;

#[tokio::main]
async fn main() -> tonin::Result<()> {
    // Telemetry (OTLP tracing + log subscriber) is installed by Service::new.
    Service::new("greeter").run().await
}
```

A real service adds `.handler(my_impl)` before `.run()` — pass an instance of a tonic-generated server (e.g. `GreeterServer::new(MyGreeter::default())`).

### `Cargo.toml`

```toml
[package]
name = "greeter"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "greeter"
path = "src/main.rs"

[dependencies]
tonin = { workspace = true }
tokio   = { workspace = true }
tonic   = { workspace = true }
prost   = { workspace = true }

[build-dependencies]
tonin-build = { workspace = true }
```

That's the whole service surface. See the full sample at <https://github.com/Rushit/tonin/tree/main/examples/greeter>.

## Service builder API

Every method on `Service` is chainable; each returns `Self`.

| Method | What it does |
| --- | --- |
| `Service::new(name)` | Construct the builder. Binds `0.0.0.0:50051` by default. Initializes OTLP tracing as a side effect (idempotent; off if `TONIN_TELEMETRY=off`). Errors during telemetry init are logged but do not block startup. |
| `.addr(SocketAddr)` | Override the gRPC bind address. |
| `.handler(svc)` | Attach a tonic-generated service (anything implementing `tonic::server::NamedService` and `tower::Service` over `tonic::body::BoxBody`). Repeatable — call once per gRPC service you want to host. The first call installs the trace-extract + auth layers; subsequent calls share them. |
| `.with_auth(verifier)` | Install a `TokenVerifier` with the default `BearerHeaderExtractor`. Every request runs extract → verify; the resulting `AuthCtx` is placed in request extensions and the `CURRENT_AUTH` task-local. |
| `.with_auth_layer(layer)` | Install a fully-customized `AuthLayer` for non-default token extraction (cookies, custom headers). |
| `.without_auth()` | Explicit anonymous mode. Handlers receive `AuthCtx { kind: PrincipalKind::Anonymous, .. }`. Use for public read-only APIs or internal-mesh services where mTLS is the only check. |
| `.enable_mcp()` | Spawn the in-process MCP listener on `0.0.0.0:50052`. Default handler answers the framework's health tool. Same process, shared `AuthCtx` task-local, shared lifecycle with gRPC. |
| `.enable_mcp_with(factory)` | Enable MCP with a custom handler — typically a `#[mcp_expose]`-generated adapter that exposes each gRPC method as an MCP tool. `factory` is called once per MCP session. |
| `.mcp_addr(SocketAddr)` | Override the MCP bind address (bind `:0` in tests for a random free port). |
| `.run()` | Serve. Spawns the MCP listener (if enabled) in parallel with the gRPC server, awaits shutdown, flushes spans on exit. Returns `Err(Config(...))` if no `.handler(...)` was registered. |

If neither `.with_auth(...)` nor `.without_auth()` is called, a permissive anonymous auth layer is installed automatically so handlers can always read `AuthCtx::from(&req)`.

## Layering

Every inbound RPC passes through two installed layers before reaching your handler:

```mermaid
sequenceDiagram
    participant Caller
    participant Extract as ExtractLayer (telemetry)
    participant Auth as AuthLayer
    participant Handler as Your handler

    Caller->>Extract: gRPC request + traceparent header
    Extract->>Extract: parse W3C TraceContext, open server span
    Extract->>Auth: request (now inside trace span)
    Auth->>Auth: BearerHeaderExtractor reads Authorization
    Auth->>Auth: TokenVerifier validates token
    Auth->>Handler: request + AuthCtx (in extensions + task-local)
    Handler->>Handler: business logic; reads AuthCtx::from(&req)
    Handler->>Auth: Response
    Auth->>Extract: Response
    Extract->>Caller: Response (span closed, exported via OTLP)
```

Order matters: telemetry wraps auth so the auth-verify work shows up as a child of the request span. If auth fails, the rejection is still recorded inside the trace.

## See also

- [04-mcp-exposure.md](04-mcp-exposure.md) — turn each gRPC method into an MCP tool with `#[mcp_expose]`.
- [05-telemetry.md](05-telemetry.md) — what `ExtractLayer` does and how spans propagate to downstream peers.
- [06-authentication.md](06-authentication.md) — `TokenVerifier`, `AuthCtx`, and the JWT default.
