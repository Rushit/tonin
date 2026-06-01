# tonin

Opinionated Rust microservice framework. Kubernetes-native, mesh-secured, MCP-by-default.

## Most users want this crate

`tonin` is the umbrella. It re-exports everything a service typically needs —
the `Service` builder, transport, discovery, MCP, telemetry, auth, and the
`prelude` — so a service `Cargo.toml` only needs one line:

```toml
[dependencies]
tonin = "0.1"
```

If you want lighter dependencies:

- [`tonin-core`](https://crates.io/crates/tonin-core) — the same types, without the umbrella indirection. Use directly if you're embedding the framework in a library and want one fewer crate in your dep tree.
- [`tonin-client`](https://crates.io/crates/tonin-client) — peer-service client primitives only. No server, no tonic-router, no telemetry init. For callers that just need to dial other services.

## Quick example

```rust,no_run
use tonin::prelude::*;

#[tokio::main]
async fn main() -> tonin::Result<()> {
    // Telemetry (OTLP tracing + log subscriber) is installed by Service::new.
    Service::new("greeter")
        // .handler(GreeterServer::new(MyImpl))  // tonic-generated service
        .run()
        .await
}
```

Once you wire a tonic-generated `handler(...)`, that's a full service:
gRPC on `0.0.0.0:50051`, W3C trace-context propagation on every request,
an optional MCP sidecar via `.enable_mcp()`, auth via `.with_auth(...)`.

See [`examples/greeter`](https://github.com/Rushit/tonin/tree/main/examples/greeter)
in the workspace for the end-to-end shape (proto + `build.rs` + `tonin.toml`).

## Re-exports

| Path                    | What it gives you                                                                  |
| ----------------------- | ---------------------------------------------------------------------------------- |
| `tonin::Service`      | The service builder. `Service::new(...).handler(...).run().await`.                 |
| `tonin::Error`        | Framework error type. Use with `tonin::Result<T>`.                               |
| `tonin::State`        | Typed shared-state container for handlers.                                         |
| `tonin::core`         | All of [`tonin-core`](https://crates.io/crates/tonin-core).                    |
| `tonin::transport`    | tonic/gRPC wiring (server builder, trace propagation layer).                       |
| `tonin::discovery`    | k8s DNS-based service resolution (`<svc>.<ns>.svc.cluster.local`).                 |
| `tonin::mcp`          | MCP listener config + `McpServerHandler` trait.                                    |
| `tonin::telemetry`    | OTLP tracing init + shutdown.                                                      |
| `tonin::auth`         | `TokenVerifier`, `AuthLayer`, `AuthCtx`, `PrincipalKind`, JWT helpers.             |
| `tonin::job`          | Background job primitives.                                                         |
| `tonin::state`        | State module (re-export of `core::state`).                                         |
| `tonin::prelude`      | Glob-imported essentials: `Service`, `Error`, `State`, `AuthCtx`, `Request`, etc.  |
| `tonin::mcp_expose`   | Proc-macro attribute. Place on a gRPC `impl` to auto-derive an MCP tool adapter.   |
| `tonin::main`         | Re-export of `tokio::main`. `#[tonin::main]` is a placeholder for the eventual attribute macro. |

`Result` is intentionally NOT in the prelude — it would shadow `std::result::Result`
and break macro-generated code. Write `tonin::Result<T>` explicitly when you want it.

## CLI

This crate is **dual-purpose** — library + binary. `cargo install tonin`
installs the `tonin` command (scaffold services, run codegen, render k8s
manifests). The CLI deps live behind the `cli` feature (default-on), so
library-only consumers should pin:

```toml
[dependencies]
tonin = { version = "0.1", default-features = false }
```

## Companion crates

- [`tonin-build`](https://crates.io/crates/tonin-build) — `build.rs` helper wrapping `tonic-build` with tonin conventions.
- [`tonin-mcp-macros`](https://crates.io/crates/tonin-mcp-macros) — the proc-macro crate behind `#[mcp_expose]`.

## Status

Pre-0.1. The public surface is still moving — pin exact versions in
production and read the [workspace repository](https://github.com/Rushit/tonin)
for the current authoritative shape.

## MSRV

Rust 1.90 (edition 2024).

## License

Apache-2.0
