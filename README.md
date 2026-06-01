# tonin

Build gRPC microservices for Kubernetes in Rust — without writing the
Dockerfile, the k8s YAML, the OTel wiring, or the LLM-tool plumbing.

[![crates.io](https://img.shields.io/crates/v/tonin.svg)](https://crates.io/crates/tonin)
[![docs.rs](https://img.shields.io/docsrs/tonin)](https://docs.rs/tonin)
[![CI](https://img.shields.io/github/actions/workflow/status/Rushit/tonin/ci.yml?branch=main)](https://github.com/Rushit/tonin/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

tonin provides the core requirements for building gRPC microservices on
Kubernetes — transport, telemetry, service discovery, and an MCP tool surface.
The tonin philosophy is sane defaults with a pluggable architecture: you get
Dockerfiles, k8s manifests, and OTel wiring out of the box, and every capability
(cache, database, event bus) swaps out with a single line in `tonin.toml`.

## Install the CLI

Three install options for `tonin` — pick whichever fits your machine. Pre-built archives are published for Linux (x86_64, ARM64), macOS (Intel, Apple Silicon), and Windows (x86_64) on every release.

```bash
# 1. Pre-built binary (fastest, no compile)
#    Browse: https://github.com/Rushit/tonin/releases/latest
TARGET=x86_64-unknown-linux-gnu  # see crates/tonin/README.md for all targets
curl -L "https://github.com/Rushit/tonin/releases/latest/download/tonin-${TARGET}.tar.gz" \
  | tar -xz -C /usr/local/bin tonin

# 2. cargo-binstall (downloads the same pre-built archive)
cargo binstall tonin

# 3. cargo install (builds from source)
cargo install tonin
```

## 30-second tour

```bash
tonin service new greeter
cd greeter
cargo run -p greeter
```

You now have a gRPC server on `:50051`, an MCP server on `:50052` exposing
every RPC as a callable tool, OTLP tracing flowing to your collector,
W3C trace-context propagated across calls, and a `./k8s/` directory of
Deployment / Service / HPA / Ingress manifests rendered for your mesh of
choice. None of that needed code.

## What you write vs. what's already done for you

You write:

- one `.proto` per service — the wire contract
- one `impl` of the generated trait — your handlers
- one `tonin.toml` — service name, replicas, mesh, capabilities

You don't write:

| Concern | Where it comes from |
| --- | --- |
| `Dockerfile` | `tonin service new` |
| gRPC server boilerplate (tonic Router, layers, bind, graceful shutdown) | `Service::new(...).handler(...).run()` |
| MCP tool surface (one tool per gRPC method, JSON-schema'd) | `#[tonin::mcp_expose]` on the impl block + `.enable_mcp_with(...)` on the `Service` builder (the scaffold wires both) |
| OTLP tracing + log subscriber init | `Service::new(...)` (zero config; reads `OTEL_*` env) |
| W3C `traceparent` extract on inbound + inject on outbound | the extract layer is installed when you call `.handler(...)`; the generated client SDK injects on outbound |
| Auth: bearer extraction, JWT validation, JWKS fetch, `AuthCtx` task-local | `.with_auth(JwtValidator::from_env()?)` |
| k8s Deployment / Service / HPA / Ingress | `tonin k8s generate` from `tonin.toml` |
| Service-mesh overlays (Cilium / Istio / Linkerd) | `[deploy].mesh = "..."` in `tonin.toml` |
| StatefulSets for `[database]` + `[cache]` (Postgres / Redis defaults) | declared in `tonin.toml`, rendered from templates |
| Multi-language client SDKs (Rust / Python / TS) from the same proto | `tonin service new --client-lang ts` etc. |
| mTLS, retries, circuit-breaking, cross-cluster routing | delegated to the service mesh — not in the framework |

`tonin.toml` is the source of truth. Re-run `tonin k8s generate` after
editing it; don't hand-edit the YAML in `./k8s/`.

## Everyday commands

```bash
tonin service new <name>        # scaffold a service (Rust / Python / TS)
tonin proto generate            # re-run codegen after editing .proto
tonin k8s generate              # render k8s/ from tonin.toml
tonin k8s validate              # kubectl apply --dry-run=server
tonin k8s diff                  # kubectl diff against current context
tonin k8s apply                 # render + kubectl apply
tonin k8s apply --workspace     # render every tonin.toml under a path
```

The CLI binary is `tonin`. See `tonin --help` for the full surface.

## Example: hello-world service

`proto/greeter.proto`:

```proto
syntax = "proto3";
package greeter.v1;

service Greeter {
  rpc SayHello (HelloRequest) returns (HelloReply);
}

message HelloRequest { string name = 1; }
message HelloReply   { string message = 1; }
```

`build.rs`:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonin_build::compile(&["proto/greeter.proto"], &["proto"])
}
```

`src/main.rs` (the shape `tonin service new` scaffolds — `tonic-build` emits `GreeterServer` + the `Greeter` trait from the `.proto`; you implement the trait in `src/server.rs`):

```rust,ignore
use tonin::prelude::*;
use greeter_server::{
    auth,
    gen::greeter_v1_server::GreeterServer,
    server::{GreeterImpl, GreeterImplMcpAdapter},
};

#[tokio::main]
async fn main() -> tonin::Result<()> {
    // Pre-wired DB + cache from DATABASE_URL / REDIS_URL env (no-op if unset).
    let state = State::from_env().await?;
    let handler = GreeterImpl::new(state);

    // Same impl serves gRPC (:50051) and MCP (:50052).
    let mcp_handler = handler.clone();
    Service::new("greeter")
        .with_auth(auth::verifier())
        .enable_mcp_with(move || Ok(GreeterImplMcpAdapter::new(mcp_handler.clone())))
        .handler(GreeterServer::new(handler))
        .run()
        .await
}
```

`tonin.toml`:

```toml
[service]
name    = "greeter"
version = "0.1.0"
codec   = "prost"      # what runs today (tonic-build); a buffa-based `protoc-gen-micro` codegen plugin is planned

[deploy]
replicas    = 2
mesh        = "cilium" # cilium | istio | linkerd | none
mcp_sidecar = true
namespace   = "default"

[resources]
cpu    = "100m"
memory = "128Mi"
```

Full source: [`examples/greeter`](examples/greeter).

## Crate map

| Crate | Role |
| ----- | ---- |
| [`tonin`](https://crates.io/crates/tonin) | Umbrella re-export. `use tonin::prelude::*;` is what most services pull in. |
| [`tonin-core`](https://crates.io/crates/tonin-core) | `Service` builder, runtime, capability traits, auth, telemetry, MCP, transport, discovery. |
| [`tonin-client`](https://crates.io/crates/tonin-client) | Tiny peer-service client primitives. No server framework deps. |
| [`tonin-mcp-macros`](https://crates.io/crates/tonin-mcp-macros) | `#[mcp_expose]` proc-macro: auto-derives an MCP adapter from a gRPC `impl` block. |
| [`tonin-build`](https://crates.io/crates/tonin-build) | `build.rs` helper that wraps `tonic-build` with tonin conventions. |
| [`tonin`](https://crates.io/crates/tonin) | The CLI binary. `cargo install tonin`. |

## Adding capabilities

Capabilities are declared in `tonin.toml`. Traits live in `tonin-core`;
implementations live in their own crates and are picked by `engine = "..."`.
Swapping a backend is a TOML change plus a `Cargo.toml` dep flip — handler
code does not change.

```toml
[database]
engine = "postgres"
size   = "10Gi"

[cache]
engine = "redis"

[secrets]
required = ["STRIPE_API_KEY"]
```

`[database]` renders a Postgres StatefulSet + headless Service + a
credentials Secret (`DATABASE_URL` + `DATABASE_PASSWORD` env vars are
injected). `[cache]` renders a Redis StatefulSet + Service with `REDIS_URL`.
`[secrets]` parses today and surfaces the required keys to your tooling; the
renderer side (emitting a `Secret` / `ExternalSecret` for those keys) lands
in a follow-up — until then, populate the Secret out-of-band. Auth is
configured via env vars (`TONIN_AUTH_ISSUER`, `TONIN_AUTH_AUDIENCE`,
`TONIN_AUTH_JWKS_URL`) and `.with_auth(JwtValidator::from_env()?)` on the
`Service` builder; a dedicated `[auth]` TOML section is roadmapped.
See [`docs/01-principles.md`](docs/01-principles.md) for the interface-first
design rationale, and [`docs/07-cache.md`](docs/07-cache.md) through
[`docs/10-secrets.md`](docs/10-secrets.md) for each capability's trait, TOML
schema, and status (what ships today vs. 0.2+).

## Status

Beta — serving production traffic. The public API may still change
before GA; pin exact versions and watch the release notes. Each
capability doc under [`docs/`](docs/) has its own Status block listing
what ships today vs. what's planned next.

mTLS, retries, circuit-breaking, and cross-cluster routing are
intentionally delegated to the service mesh — not implemented in the
framework crates.

## Releasing

The workspace ships every published crate from a single source version.
To cut a new release:

```bash
make release VERSION=0.2.0
# ... review the bump commit, then:
git push origin main
git push origin v0.2.0
```

Pushing the `vX.Y.Z` tag triggers `.github/workflows/release.yml`,
which runs `make publish` (publishes all six crates to crates.io in
dep order), builds cross-platform CLI binaries, and creates a GitHub
Release with binaries + auto-generated notes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for dev setup, PR conventions, and the scope of changes welcome at this stage. We follow [Contributor Covenant 2.1](CODE_OF_CONDUCT.md).

## Security

To report a vulnerability, follow [SECURITY.md](SECURITY.md). **Don't open a public issue with exploitable details** — use the `[security]` tag flow described there so a maintainer can reach out privately.

## License

Apache-2.0. See [LICENSE](LICENSE).
