# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

The reader-facing documentation lives at [`docs/00-overview.md`](docs/00-overview.md) and the capability docs alongside it (`docs/01-principles.md` through `docs/16-config.md`). That tree is the authoritative source for "what tonin does and why" — start there before redesigning a capability or `tonin.toml` section.

## Agent workflow (branches, commits, PRs)

When making changes in this repo, always work on a branch and open a PR — never
commit to `main` directly.

- **One branch per work item.** Branch off the latest `origin/main`
  (`git fetch origin main && git switch -c <type>/<short-name> origin/main`).
  Keep unrelated changes on separate branches/PRs; don't stack them.
- **Open a PR for review, don't self-merge.** `gh pr create --base main`, then
  leave it for the maintainer to approve and merge.
- **Concise commit messages.** Conventional Commits (`feat:`, `fix:`, `ci:`,
  `test:`, `docs:`, `chore:`) with a one-line subject; add a body only when it
  explains *why*. Do **not** add a `Co-authored-by` trailer.
- **Green before pushing.** Run `make ci` (the pre-commit hook also runs it).

## Common commands

Pinned toolchain: Rust `1.90` with `rustfmt` + `clippy` (`rust-toolchain.toml`).

```bash
# Build / check the whole workspace
cargo build
cargo check --workspace --all-targets

# Lint + format
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

# Tests — entire workspace, one crate, or a single test
cargo test --workspace
cargo test -p tonin-sdk
cargo test -p tonin codegen::
cargo test -p tonin codegen::plan::tests::name_of_test -- --nocapture

# Run the CLI (binary `tonin`, built from the umbrella crate `crates/tonin`)
cargo run -p tonin -- service new greeter
cargo run -p tonin -- proto generate
cargo run -p tonin -- k8s generate
```

The CLI binary is named `tonin` and ships from the `crates/tonin` package, which also re-exports `tonin-plugin` for the codegen layer. The package at `crates/tonin` produces `target/debug/tonin`.

## Architecture

The repo is one Cargo workspace split into three roles: **framework crates** (what services depend on at runtime), the **CLI** (what developers run at build / deploy time), and **templates + examples** (what the CLI renders from).

### Framework crates (`crates/`)

The workspace was restructured pre-0.1 from ~10 narrow crates to **Shape B: 6 library crates + 1 CLI binary** to reduce publish/version-bump overhead. What used to be `tonin-transport`, `tonin-discovery`, `tonin-mcp`, and `tonin-telemetry` are now modules inside `tonin-sdk`; `tonin-codegen` moved into the CLI as `crates/tonin/src/codegen/`. At 0.5 the `tonin-plugin` bridge crate was extracted from the CLI so plugin authors get a minimal `tonin.toml` API without pulling in the full CLI dep tree. See [`docs/02-architecture.md`](docs/02-architecture.md) for the current shape.

Services depend on `tonin-sdk` directly. The library crates are:

- `tonin-sdk` — the runtime framework (renamed from `tonin-core` at 0.5). `Service`, `Config`, `Context`, `Error`, plus the `transport` (tonic/gRPC wiring; **mTLS delegated to the service mesh**), `discovery` (resolves `<service>.<ns>.svc.cluster.local` via k8s DNS; cross-cluster routing is the mesh's job), `mcp` (MCP sidecar runtime forwarding MCP tool calls to a co-located gRPC service), `telemetry` (zero-config OTLP, opentelemetry 0.27 stack), and `auth` modules. **Capability traits (e.g. `Cache`, `EventBus`) live here.** Implementations live in their own crates (0.2+) and are selected by `tonin.toml` (`engine = "..."`). See [`docs/01-principles.md`](docs/01-principles.md) "Interface-first capabilities" — this is the load-bearing convention. Don't put a concrete backend in `tonin-sdk`.
- `tonin-plugin` — minimal Plan API for plugin authors (~4 deps: serde, toml, thiserror, walkdir). Exposes `Plan::load_with_env()`, `select_env()`, all resolved types, and `RECOMMENDED_CLI_MIN`. No clap/tera/include_dir. What `tonin-helm` and other CLI plugins depend on.
- `tonin-client` — tiny peer-service client primitives, with no server-framework deps. Lets a service depend on another service's generated client without pulling in tokio/tonic-server.
- `tonin-mcp-macros` — proc-macro crate exposing `#[mcp_expose]`, which auto-derives MCP tool definitions from a gRPC `impl` block.
- `tonin-build` — `build.rs` helper wrapping `tonic-build` with tonin conventions; used by generated services.

### CLI (`crates/tonin/`)

`main.rs` dispatches to three subcommand groups in `src/commands/`:

- `service` — scaffold (`service new <name>`), run locally
- `proto` — codegen from `.proto` files (drives the in-tree `codegen` module)
- `k8s` — render / validate / diff / apply manifests

The codegen engine lives at `crates/tonin/src/codegen/` (templating via Tera + `include_dir`): `plan.rs` and `stateful.rs` are thin `pub use tonin_plugin::...` re-exports — the actual Plan parsing logic lives in `tonin-plugin`. `render.rs` does the Tera rendering. The codegen lives in the CLI because nothing else needs tera/include_dir.

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
- **`tonin.toml` is versioned (top-level `schema = "v1"`) and backward-compatible by default.** Files written by older CLIs keep parsing — a missing `schema` field is treated as `v1`. **Adding fields to v1 must be additive only** (new optional fields with defaults that match today's behavior). Renaming, removing, or retyping a field requires bumping `CURRENT_SCHEMA` in `crates/tonin/src/codegen/plan.rs` AND writing the migration in the same change. This section is the policy; `CURRENT_SCHEMA` in `plan.rs` is the enforcing source of truth.
- **Capability sections in `tonin.toml` (`[cache]`, `[database]`, future `[eventbus]`) select an implementation via `engine = "..."`.** Swapping Redis → NATS for events should be a TOML change + a `Cargo.toml` dep flip, never a handler rewrite. Preserve that invariant when adding new capabilities.
- **Mesh-delegated concerns** (mTLS, retries, circuit breaking, cross-cluster routing) are intentionally absent from the framework crates. See [`docs/13-service-mesh.md`](docs/13-service-mesh.md) and [`docs/01-principles.md`](docs/01-principles.md) (mesh-delegated network concerns) for the rationale. Don't reintroduce them in `tonin-sdk::transport` / `tonin-sdk::discovery` without a deliberate design pass.
- **Codec today is prost** via `tonic-build` — every scaffolded service and every example compiles its `.proto` through prost. The `[service].codec` field in `tonin.toml` and the `TONIN_CODEC` env var reserve the surface for a future `protoc-gen-micro` (buffa-based) plugin; setting `codec = "buffa"` is a no-op today and falls back to prost with a stderr notice.

## How to add a feature

tonin is opinionated. A new feature should fit the four principles or name the deviation
explicitly. Reason in this order:

1. **Does it fit the four principles?** (full text in [`docs/01-principles.md`](docs/01-principles.md))
   - **Interface-first** — capabilities are traits in `tonin-sdk`; concrete backends live in
     their own crates, selected by `engine = "..."`. Define the trait before any backend.
   - **Mesh-delegated** — mTLS, retries, circuit breaking, cross-cluster routing belong to the
     mesh, not the framework. Don't build them in.
   - **MCP-by-default** — anything exposing RPCs must keep the "every method is also a tool"
     property intact.
   - **`tonin.toml` is the single source of truth** — new config is a `tonin.toml` field, and the
     CLI renders from it. No second source, no hand-edited generated output.
2. **Which layer owns it?** A new capability → a trait in `tonin-sdk` (+ a separate impl crate),
   never a concrete backend inside `tonin-sdk`. A new build/deploy action → a command under
   `crates/tonin/src/commands/` + a `TopCmd` variant. New generated output → a Tera template + a
   `plan.rs` decision + a test.
3. **Is it additive to the schema?** New `tonin.toml` fields must be optional with defaults that
   preserve today's behavior. Otherwise bump `CURRENT_SCHEMA` and write the migration in the same
   change.
4. **Read and update the governing doc.** Each capability has a `docs/NN-*.md` with a Status block
   (ships-now vs. roadmap) — read it before designing, update it in the same change.

## Rust standards

**Clean interfaces and simplicity come first.** Readable, maintainable, extensible code beats clever
code. Apply the optimizations below only when they don't cost clarity — never contort an API to save
an allocation.

- **Async-first.** The runtime is tokio. I/O-bearing capability methods are `async`. Never block the
  runtime — no blocking `std::fs`/`std::net` or CPU-heavy loops on async paths (use `tokio`
  equivalents or `spawn_blocking`), and don't hold a lock across `.await`.
- **Errors, not panics.** Library crates (`tonin-sdk`, `tonin-plugin`, …) return typed errors (`thiserror`,
  `tonin::Result`) — no `unwrap`/`expect`/`panic!` on a reachable path; a panic in a service kills
  the pod. The CLI uses `anyhow`/`bail!` with actionable messages. Propagate with `?` and add
  context; don't swallow errors.
- **Zero-copy where it's free.** Borrow (`&str`, `&[u8]`) over owning in signatures; pass buffers as
  `bytes::Bytes` (cheap clone); avoid reflexive `.clone()`/`.to_string()`. But a `String` that makes
  a signature obvious beats a lifetime-tangled `&'a str`, and CLI/codegen paths are cold — clone
  freely there.
- **Pluggable over monomorphized.** A `dyn Capability` trait object is the right call for swappable
  backends even though generics would be marginally faster — the clean, swappable interface is the
  point.
- **Crash-safe, deterministic codegen.** Write generated files atomically (temp + rename) so an
  interrupted `tonin k8s generate` / `proto generate` never leaves partial output. Rendered output
  must be deterministic (stable ordering) so re-running produces no spurious diffs.
- **No `unsafe`.** Nothing here needs it.

## Definition of done

Drive the gate through `make` (run `make help` for the full target table):

- **`make fmt` then `make ci`** passes — `ci` is the same gate CI runs: `fmt-check` + clippy
  `-D warnings` + `test` + `doc`. Zero warnings is a gate, not advice.
- New `plan.rs`/codegen behavior has a unit test (plain `#[test]` / `#[tokio::test]` — there is no
  snapshot harness; don't add one without reason).
- Touched a capability or `tonin.toml` field? Update its `docs/NN-*.md` + Status block in the same
  change. Touched a template under `crates/tonin/templates/` or the codegen module? Run
  **`make gen-example`** (re-renders the greeter manifests and confirms it builds).
- Commit messages follow Conventional Commits (`feat:`, `fix:`, …); see `CONTRIBUTING.md`.

## Doc map

The 17-file `docs/` tree (`00`–`16`) is the authoritative reader documentation — check the relevant
doc before designing a new `tonin.toml` section, capability trait, or framework concept.

- [`docs/01-principles.md`](docs/01-principles.md) — the four rules above, in full.
- [`docs/02-architecture.md`](docs/02-architecture.md) — crate map, runtime layout, request flow.
- `03`–`06` — build-a-service flow (Service builder, MCP exposure, telemetry, auth).
- `07`–`10`, `16` — capability traits (Cache, Database, EventBus, SecretStore, Config).
- `11`–`13` — deploy story (DNS discovery, k8s rendering, mesh overlays).
- `14`–`15` — background jobs, polyglot scaffolding.
