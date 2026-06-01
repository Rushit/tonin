# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

Pre-0.1, preparing first crates.io publish. The runtime, CLI, and example services compile and run. Expect commands, crate APIs, and templates to move before 0.1.0; prefer reading the current code over README phrasing when they disagree.

The reader-facing documentation lives at [`docs/00-overview.md`](docs/00-overview.md) and the 15 capability docs alongside it (`docs/01-principles.md` through `docs/15-multi-language.md`). That tree is the authoritative source for "what tonin does and why" — start there before redesigning a capability or `tonin.toml` section.

## Common commands

Pinned toolchain: `stable` with `rustfmt` + `clippy` (`rust-toolchain.toml`).

```bash
# Build / check the whole workspace
cargo build
cargo check --workspace --all-targets

# Lint + format
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

# Tests — entire workspace, one crate, or a single test
cargo test --workspace
cargo test -p tonin-core
cargo test -p tonin codegen::
cargo test -p tonin codegen::plan::tests::name_of_test -- --nocapture

# Run the CLI (binary `tonin`, built from the umbrella crate `crates/tonin`)
cargo run -p tonin -- service new greeter
cargo run -p tonin -- proto generate
cargo run -p tonin -- k8s generate
```

The CLI binary is named `tonin` and ships from the same `tonin` umbrella crate as the library re-exports (feature-gated under `cli`, default-on). The package at `crates/tonin` produces `target/debug/tonin`.

## Architecture

The repo is one Cargo workspace split into three roles: **framework crates** (what services depend on at runtime), the **CLI** (what developers run at build / deploy time), and **templates + examples** (what the CLI renders from).

### Framework crates (`crates/`)

The workspace was restructured pre-0.1 from ~10 narrow crates to **Shape B: 5 library crates + 1 CLI binary** to reduce publish/version-bump overhead. What used to be `tonin-transport`, `tonin-discovery`, `tonin-mcp`, and `tonin-telemetry` are now modules inside `tonin-core`; `tonin-codegen` moved into the CLI as `crates/tonin/src/codegen/`. See [`docs/02-architecture.md`](docs/02-architecture.md) for the current shape.

Services consume the umbrella crate `tonin`, which re-exports `tonin-core`. The library crates are:

- `tonin` — umbrella re-export crate. What most services depend on. Thin: re-exports `tonin-core` (and selected helpers) so downstream `Cargo.toml` stays a one-liner.
- `tonin-core` — the framework. `Service`, `Config`, `Context`, `Error`, plus the `transport` (tonic/gRPC wiring; **mTLS delegated to the service mesh**), `discovery` (resolves `<service>.<ns>.svc.cluster.local` via k8s DNS; cross-cluster routing is the mesh's job), `mcp` (MCP sidecar runtime forwarding MCP tool calls to a co-located gRPC service), `telemetry` (zero-config OTLP, opentelemetry 0.27 stack), and `auth` modules. **Capability traits (e.g. `Cache`, `EventBus`) live here.** Implementations will live in their own crates (0.2+) and are selected by `tonin.toml` (`engine = "..."`). See [`docs/01-principles.md`](docs/01-principles.md) "Interface-first capabilities" — this is the load-bearing convention. Don't put a concrete backend in `tonin-core`.
- `tonin-client` — tiny peer-service client primitives, with no server-framework deps. Lets a service depend on another service's generated client without pulling in tokio/tonic-server.
- `tonin-mcp-macros` — proc-macro crate exposing `#[mcp_expose]`, which auto-derives MCP tool definitions from a gRPC `impl` block.
- `tonin-build` — `build.rs` helper wrapping `tonic-build` with tonin conventions; used by generated services.

### CLI (`crates/tonin/`)

`main.rs` dispatches to three subcommand groups in `src/commands/`:

- `service` — scaffold (`service new <name>`), run locally
- `proto` — codegen from `.proto` files (drives the in-tree `codegen` module)
- `k8s` — render / validate / diff / apply manifests

The codegen engine lives at `crates/tonin/src/codegen/` (templating via Tera + `include_dir`): `plan.rs` decides *what* to render from a parsed proto + `tonin.toml`; `render.rs` does the rendering; `stateful.rs` handles the `[database]` / `[cache]` capability blocks. It was folded into the CLI binary because nothing else needs to depend on it.

Adding a new top-level command means a new module under `commands/` plus a variant in the `TopCmd` enum in `main.rs`.

### Templates (`crates/tonin/templates/`)

The CLI does not generate code from scratch — it renders Tera templates with values from a parsed `tonin.toml` + proto descriptor. Templates ship inside the CLI crate (embedded via `include_dir`), not at the workspace root.

- `crates/tonin/templates/service/{rust,python,ts,_shared}` — service skeletons. The Rust skeleton has its own `build.rs.tmpl` and `Cargo.toml.tmpl`.
- `crates/tonin/templates/k8s/*.yaml.tmpl` — one template per k8s resource (Deployment, Service, HPA, Ingress, plus stateful deps: `db-*`, `cache-*`, secrets).
- `crates/tonin/templates/k8s/mesh/` — mesh-specific overlays (Cilium / Istio / Linkerd, selected by `[deploy].mesh` in `tonin.toml`).
- `crates/tonin/templates/Dockerfile.tmpl` — language-agnostic Dockerfile.

Generated k8s output lands under `examples/*/k8s/` and is gitignored.

### Examples (`examples/`)

Each example is a workspace member, not a standalone crate. `examples/greeter/` is the canonical end-to-end shape: one `proto/*.proto`, one `src/main.rs`, one `tonin.toml` (`[service]`, `[deploy]`, `[resources]`), and a `build.rs` that calls `tonin-build`.

## Authoring conventions worth knowing

- **`tonin.toml` is the single source of truth.** Service name, mesh choice, replicas, resources, MCP sidecar toggle, stateful deps — all here. The CLI re-renders k8s manifests from it; don't hand-edit generated YAML.
- **`tonin.toml` is versioned (top-level `schema = "v1"`) and backward-compatible by default.** Files written by older CLIs keep parsing — a missing `schema` field is treated as `v1`. **Adding fields to v1 must be additive only** (new optional fields with defaults that match today's behavior). Renaming, removing, or retyping a field requires bumping `CURRENT_SCHEMA` in `crates/tonin/src/codegen/plan.rs` AND writing the migration in the same change. See [`docs/17-schema-versioning.md`](docs/17-schema-versioning.md) for the policy coding agents must follow.
- **Capability sections in `tonin.toml` (`[cache]`, `[database]`, future `[eventbus]`) select an implementation via `engine = "..."`.** Swapping Redis → NATS for events should be a TOML change + a `Cargo.toml` dep flip, never a handler rewrite. Preserve that invariant when adding new capabilities.
- **Mesh-delegated concerns** (mTLS, retries, circuit breaking, cross-cluster routing) are intentionally absent from the framework crates. See [`docs/13-service-mesh.md`](docs/13-service-mesh.md) and [`docs/01-principles.md`](docs/01-principles.md) (mesh-delegated network concerns) for the rationale. Don't reintroduce them in `tonin-core::transport` / `tonin-core::discovery` without a deliberate design pass.
- **Codec today is prost** via `tonic-build` — every scaffolded service and every example compiles its `.proto` through prost. The `[service].codec` field in `tonin.toml` and the `TONIN_CODEC` env var reserve the surface for a future `protoc-gen-micro` (buffa-based) plugin; setting `codec = "buffa"` is a no-op today and falls back to prost with a stderr notice.

## Capability docs

The 16-file `docs/` tree is the authoritative reader documentation. Before designing a new `tonin.toml` section, capability trait, or framework concept, check the relevant doc:

- [`docs/00-overview.md`](docs/00-overview.md) — landing page; what tonin is, who it's for, doc navigation.
- [`docs/01-principles.md`](docs/01-principles.md) — interface-first, mesh-delegated, MCP-by-default, `tonin.toml` as source of truth. Any new design should fit these four rules; if it doesn't, name the deviation explicitly.
- [`docs/02-architecture.md`](docs/02-architecture.md) — current crate map, runtime layout, request flow.
- `docs/03-grpc-service.md` through `docs/06-authentication.md` — the build-a-service flow (Service builder, MCP exposure, telemetry, auth).
- `docs/07-cache.md` through `docs/10-secrets.md` — capability traits (Cache, Database, EventBus, SecretStore).
- `docs/11-service-discovery.md` through `docs/13-service-mesh.md` — deploy story (DNS, k8s rendering, mesh overlays).
- `docs/14-background-jobs.md`, `docs/15-multi-language.md` — beyond gRPC: jobs and polyglot scaffolding.

Each capability doc has a "Status" section flagging what ships in 0.1 vs. what's deferred. Use those status callouts to decide whether a feature exists today or is on the roadmap before you start writing code.
