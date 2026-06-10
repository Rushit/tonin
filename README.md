# tonin

**The one-person framework for Kubernetes microservices.**

gRPC, observability, auth, and an LLM tool surface — wired from a single `tonin.toml`.
Built for developers who are also their own ops team.

[![crates.io](https://img.shields.io/crates/v/tonin.svg)](https://crates.io/crates/tonin)
[![docs.rs](https://img.shields.io/docsrs/tonin)](https://docs.rs/tonin)
[![CI](https://img.shields.io/github/actions/workflow/status/Rushit/tonin/ci.yml?branch=main)](https://github.com/Rushit/tonin/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

tonin is an opinionated Rust framework for teams that don't have a platform team.
It gives you the *whole* path from `.proto` to a deployed, observable, mesh-ready
service — without the week of YAML, Dockerfiles, OTel wiring, and MCP plumbing that
usually stands between an idea and a running endpoint.

The philosophy is **sane defaults with a pluggable architecture**: every batteries-included
default works on day one, and every capability — cache, database, event bus, config —
swaps to another backend with a single line in `tonin.toml`. No handler rewrite, no lock-in.

---

## Why tonin

If you're a solo developer or a small team, your scarcest resource is *you*. tonin is
built around that constraint:

- **One `.proto` in, a deployable service out.** The contract is the only thing you
  have to design. Everything downstream — server boilerplate, container, manifests,
  telemetry — is generated from it.
- **No platform tax.** The infrastructure decisions a 50-person org pays an SRE team to
  make (how to wire tracing, where mTLS lives, how the HPA scales) ship as defaults you
  can override, not blank files you have to fill in.
- **Swap, don't rewrite.** Capabilities are interfaces. Redis today, something else
  tomorrow is a TOML change and a dependency flip — your handlers never know.
- **AI-native by default.** Every gRPC method is automatically a callable MCP tool, so
  your service is usable by an LLM agent the moment it's usable by a client.

For those counting — the commands to get a running, instrumented, MCP-enabled gRPC
service are:

```bash
# 1. install the CLI (install script — fastest, no compile)
curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh \
  | bash -s -- --with-tonin-helm
# or: cargo install tonin

tonin service new greeter  # 2. scaffold
cd greeter && cargo run    # 3. run
```

That's it. A gRPC server on `:50051`, an MCP server on `:50052` exposing every RPC as a
tool, OTLP traces flowing to your collector, and a `./k8s/` directory rendered for your
mesh — none of which you wrote.

---

## Capabilities

tonin ships the core building blocks of a distributed system as first-class, swappable
capabilities. Each works out of the box and is selected or reconfigured in `tonin.toml`.

- **gRPC Transport** — tonic/tower server wiring, graceful shutdown, and a generated
  typed client, all from your `.proto`. You implement one trait; the Router, layers, and
  bind are handled.
- **MCP Tool Surface** — annotate a handler with `#[mcp_expose]` and every gRPC method
  becomes a JSON-schema'd, LLM-callable tool served by a co-located MCP sidecar. No
  separate MCP server to maintain.
- **Telemetry** — zero-config OpenTelemetry. OTLP traces and structured logs with W3C
  trace-context propagated across service calls, configured from `OTEL_*` env vars.
- **Authentication** — bearer extraction, JWT validation, JWKS fetch, and a task-local
  `AuthCtx`, wired with one `.with_auth(...)` call. Built for zero-trust networking.
- **Service Discovery** — peer services resolve over Kubernetes DNS
  (`<service>.<ns>.svc.cluster.local`). No registry to run; cross-cluster routing stays
  in the mesh.
- **Cache** — a `Cache` trait with a Redis-backed default and telemetry-wrapped
  operations. Swap the engine in `tonin.toml`.
- **Database** — a `Database` trait with a Postgres default via `sqlx`, plus a rendered
  StatefulSet, Service, and injected credentials.
- **Event Bus** — an `EventBus` trait with ack/nack semantics for async, event-driven
  communication. Redis → NATS is a config change, not a rewrite.
- **Dynamic Config** — load and hot-reload app config from env, etcd, GitHub, or chained
  sources behind one `Config` trait.
- **Secrets** — a `SecretStore` trait over an env-backed default, with required keys
  declared in `tonin.toml`.
- **Background Jobs** — `jobs::bootstrap` plus generated Kubernetes CronJobs for
  scheduled and async work.
- **Kubernetes Deploy** — Deployment, Service, HPA, and Ingress rendered from one
  `tonin.toml`, with Cilium / Istio / Linkerd mesh overlays.
- **Multi-Language Clients** — generate Rust, Python, and TypeScript service skeletons
  and client SDKs from the same `.proto`.
- **Pluggable Interfaces** — every capability above is a trait in `tonin-sdk` with
  implementations selected by `engine = "..."`. Defaults are good; nothing is welded in.

**Intentionally delegated to the service mesh:** mTLS, retries, circuit breaking, and
cross-cluster routing. tonin renders the right overlays and stays out of the network's
way rather than reimplementing what Cilium/Istio/Linkerd already do well.

---

## Install the CLI

Pre-built archives are published for Linux (x86_64, ARM64), macOS (Intel, Apple Silicon),
and Windows (x86_64) on every release.

**Install tonin + tonin-helm (recommended):**
```bash
curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh \
  | bash -s -- --with-tonin-helm
```

**Install tonin only:**
```bash
curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh \
  | bash
```

**Custom install directory:**
```bash
curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh \
  | bash -s -- --with-tonin-helm --dir /usr/local/bin
```

**Via cargo-binstall:**
```bash
cargo binstall tonin
cargo binstall tonin-helm
```

**Build from source:**
```bash
cargo install tonin
cargo install tonin-helm
```

## Update

Re-running the install script upgrades to the latest release. It detects the currently
installed version and skips the download if already up to date.

**Update tonin + tonin-helm to latest:**
```bash
curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh \
  | bash -s -- --with-tonin-helm
```

**Pin specific versions:**
```bash
curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh \
  | bash -s -- \
      --with-tonin-helm \
      --version v0.5.4 \
      --helm-version v0.1.1
```

**Via cargo-binstall:**
```bash
cargo binstall tonin
cargo binstall tonin-helm
```

## What you write vs. what tonin handles

**You write three files:**

- a `.proto` — the wire contract
- a handler `impl` — your business logic
- a `tonin.toml` — service name, replicas, mesh, capabilities

**tonin generates and wires everything else** from those three, and keeps it all in sync:

- the **Dockerfile** and the **Kubernetes manifests** (Deployment, Service, HPA, Ingress)
- the **service-mesh overlay** for Cilium, Istio, or Linkerd
- **OpenTelemetry** tracing and structured logs, with trace context propagated across calls
- the **MCP tool server** — every gRPC method exposed as an LLM-callable tool
- **JWT auth** — bearer extraction, validation, JWKS fetch, and request-scoped identity
- **StatefulSets** for your Postgres / Redis dependencies, with credentials injected
- **client SDKs** in Rust, Python, or TypeScript from the same proto

The network concerns it deliberately *doesn't* implement — mTLS, retries, circuit breaking,
cross-cluster routing — are delegated to your service mesh, where they belong.

`tonin.toml` is the source of truth: re-run `tonin k8s generate` after editing it rather than
hand-editing the YAML in `./k8s/`.

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

## Adding capabilities

Capabilities are declared in `tonin.toml`. Traits live in `tonin-sdk`;
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

## Crate map

| Crate | Role |
| ----- | ---- |
| [`tonin`](https://crates.io/crates/tonin) | Umbrella re-export. `use tonin::prelude::*;` is what most services pull in. |
| [`tonin-sdk`](https://crates.io/crates/tonin-sdk) | `Service` builder, runtime, capability traits, auth, telemetry, MCP, transport, discovery. |
| [`tonin-plugin`](https://crates.io/crates/tonin-plugin) | Minimal `tonin.toml` Plan API for plugin authors. No CLI deps. |
| [`tonin-client`](https://crates.io/crates/tonin-client) | Tiny peer-service client primitives. No server framework deps. |
| [`tonin-mcp-macros`](https://crates.io/crates/tonin-mcp-macros) | `#[mcp_expose]` proc-macro: auto-derives an MCP adapter from a gRPC `impl` block. |
| [`tonin-build`](https://crates.io/crates/tonin-build) | `build.rs` helper that wraps `tonic-build` with tonin conventions. |
| [`tonin`](https://crates.io/crates/tonin) | The CLI binary. `cargo install tonin`. |

## Documentation

Reader-facing docs live in [`docs/`](docs/00-overview.md) — start with
[00-overview.md](docs/00-overview.md), then [01-principles.md](docs/01-principles.md)
for the four design rules (interface-first, mesh-delegated, MCP-by-default,
`tonin.toml` as the single source of truth). Each capability has its own doc with a
Status block listing what ships today vs. what's planned. Per-crate API reference is on
[docs.rs](https://docs.rs).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for dev setup, PR conventions, and the scope of
changes welcome at this stage. We follow [Contributor Covenant 2.1](CODE_OF_CONDUCT.md).

## Security

To report a vulnerability, follow [SECURITY.md](SECURITY.md). **Don't open a public issue
with exploitable details** — use the `[security]` tag flow described there so a maintainer
can reach out privately.

## License

Apache-2.0. See [LICENSE](LICENSE).
</content>
</invoke>
