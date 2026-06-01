# Overview

The docs landing page — what tonin is, who should use it, and where to read next.

## What is tonin

tonin lets you build gRPC microservices for Kubernetes in Rust without writing the Dockerfile, the k8s YAML, the OpenTelemetry wiring, or the LLM tool-calling plumbing yourself. The runtime is a one-liner: `Service::new(...).handler(...).run()` boots the gRPC server, OTLP tracing, JWT auth, and (optionally) an MCP sidecar from values in `tonin.toml`. The deploy story is two commands: `tonin service new <name>` scaffolds the crate; `tonin k8s generate` renders Deployment, Service, HPA, Ingress, and mesh overlays from the same `tonin.toml`.

## Who is it for

- **Backend developers shipping gRPC services to Kubernetes.** You want tonic + tower + OTel + a Dockerfile + manifests, and you want them to agree with each other. tonin is the glue.
- **Teams already running a service mesh** (Cilium, Istio, or Linkerd). mTLS, retries, and cross-cluster routing stay in the mesh; tonin generates the right overlays and stays out of the way.
- **Teams exposing services to LLMs over MCP.** Annotate a handler with `#[mcp_expose]` and the MCP sidecar forwards tool calls to your gRPC server — no separate MCP server to maintain.

## How to navigate these docs

### Foundations

| Doc | What's in it |
| --- | --- |
| [00-overview.md](00-overview.md) | This page — what tonin is and where to read next. |
| [01-principles.md](01-principles.md) | Interface-first capabilities, mesh-delegated concerns, MCP-by-default, `tonin.toml` as the single source of truth. |
| [02-architecture.md](02-architecture.md) | Crate map, runtime layout, how the CLI, codegen, templates, and framework crates fit together. |

### Building a service

| Doc | What's in it |
| --- | --- |
| [03-grpc-service.md](03-grpc-service.md) | The `Service` builder, writing a handler, running locally. |
| [04-mcp-exposure.md](04-mcp-exposure.md) | Auto-expose gRPC methods as MCP tools with `#[mcp_expose]`. |
| [05-telemetry.md](05-telemetry.md) | Zero-config OTLP tracing, W3C TraceContext propagation, span semantics. |
| [06-authentication.md](06-authentication.md) | JWT validation, `AuthCtx`, `AuthLayer`, anonymous mode. |

### Capability traits

| Doc | What's in it |
| --- | --- |
| [07-cache.md](07-cache.md) | The `Cache` trait, Redis-backed default, telemetry-wrapped operations. |
| [08-database.md](08-database.md) | The `Database` trait, Postgres default via `sqlx`. |
| [09-event-bus.md](09-event-bus.md) | The `EventBus` trait, ack/nack semantics, subscribe options. |
| [10-secrets.md](10-secrets.md) | The `SecretStore` trait and the env-backed default. |
| [16-config.md](16-config.md) | The `Config` trait — dynamic app config from env, etcd, GitHub, or chained sources, with hot reload. |
| [17-schema-versioning.md](17-schema-versioning.md) | `tonin.toml` versioning + backward-compatibility policy for the CLI and coding agents. |

### Deploying

| Doc | What's in it |
| --- | --- |
| [11-service-discovery.md](11-service-discovery.md) | k8s DNS resolution for peer services. |
| [12-kubernetes-deploy.md](12-kubernetes-deploy.md) | Rendering Deployment, Service, HPA, Ingress, and stateful dep manifests from `tonin.toml`. |
| [13-service-mesh.md](13-service-mesh.md) | Mesh overlays for Cilium, Istio, and Linkerd. |

### Beyond gRPC

| Doc | What's in it |
| --- | --- |
| [14-background-jobs.md](14-background-jobs.md) | `jobs::bootstrap`, k8s CronJob generation. |
| [15-multi-language.md](15-multi-language.md) | Generating Rust, Python, and TypeScript service skeletons from one `.proto`. |

## 5-minute quick start

**Prerequisites.** A working Rust toolchain (1.90+), `protoc` on `PATH`,
and — for anything past `cargo run` — a reachable Kubernetes cluster.
Locally: [Rancher Desktop](https://rancherdesktop.io), OrbStack (k8s
mode), Docker Desktop, kind, k3d, or minikube. There is no embedded
cluster. See [12-kubernetes-deploy.md](12-kubernetes-deploy.md) for the
full list and the mesh-install caveat.

```bash
# Install the CLI
cargo install tonin

# Scaffold a new service
tonin service new greeter
cd greeter

# Run locally (no cluster needed)
cargo run -p greeter

# Generate and apply k8s manifests (needs a reachable cluster)
tonin k8s generate
tonin k8s apply
```

The full reference service — proto, `tonin.toml`, handler, `build.rs`, generated manifests — lives at [examples/greeter](https://github.com/Rushit/tonin/tree/main/examples/greeter).

## Need help?

- File an issue at https://github.com/Rushit/tonin/issues.
- Per-crate API reference is on [docs.rs](https://docs.rs): `tonin`, `tonin-core`, `tonin-client`, `tonin-build`, `tonin-mcp-macros`.

## See also

- [01-principles.md](01-principles.md) — the design rules the rest of the docs rely on.
- [02-architecture.md](02-architecture.md) — how the pieces fit together.
- [03-grpc-service.md](03-grpc-service.md) — start here once you've scaffolded a service.
