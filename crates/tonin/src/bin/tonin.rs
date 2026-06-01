//! The `tonin` CLI — scaffold services, generate code from `.proto`, and
//! render Kubernetes manifests for the [tonin](https://crates.io/crates/tonin)
//! framework.
//!
//! Three subcommand groups dispatched from [`TopCmd`]:
//!
//! - `tonin service new <name>` — scaffold a Rust / Python / TypeScript
//!   service from the templates baked into this binary
//!   (see `crates/tonin/templates/`).
//! - `tonin proto generate` — `.proto` codegen entry point. Today the
//!   scaffold's `build.rs` drives `tonic-build` via `tonin-build`; this
//!   command is the placeholder for the future `protoc-gen-tonin`.
//! - `tonin k8s` — render / validate / diff / apply Kubernetes manifests
//!   from a service's `tonin.toml`. The rendering pipeline lives in
//!   `tonin::codegen` (Tera + `include_dir`).
//!
//! End users install with `cargo install tonin`.

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

use tonin::commands;

#[derive(Parser)]
#[command(
    name = "tonin",
    version,
    about = "Build gRPC microservices for Kubernetes — the tonin CLI",
    long_about = "The tonin CLI scaffolds gRPC microservices, runs codegen, \
                  and renders Kubernetes manifests for them.

COMMON WORKFLOWS

  Scaffold and run locally:
    tonin service new greeter        # creates ./greeter/ with proto + impl
    cd greeter
    cargo run -p greeter               # boots gRPC + MCP locally

  Render manifests and apply to a cluster:
    tonin k8s generate               # writes ./k8s/*.yaml from tonin.toml
    tonin k8s validate               # server-side dry-run via kubectl
    tonin k8s apply                  # actually applies to the current context

  Workspace-wide deploy:
    tonin k8s apply --workspace --path ./services

INTROSPECTION FOR CODING AGENTS

  Every subcommand supports `--help` (short) and `--long-help` is what's
  printed when you pass --help to a leaf command. For a machine-readable
  manifest of every command + arg + prerequisite, use:

    tonin describe                   # text
    tonin describe --format json     # JSON

  The JSON shape is documented at the top of `tonin describe --help`.

ENVIRONMENT

  TONIN_ENV         Default overlay env for k8s render (overlaid by --env).
  TONIN_TELEMETRY   Set to 'off' to disable OTLP exports in scaffolded svcs.
  OTEL_EXPORTER_OTLP_ENDPOINT   Where scaffolded services send traces.

SEE ALSO

  docs/00-overview.md — landing page for capability docs.
  https://github.com/Rushit/tonin — source, issues, releases."
)]
struct Cli {
    #[command(subcommand)]
    cmd: TopCmd,
}

#[derive(Subcommand)]
enum TopCmd {
    /// Kubernetes operations: render, validate, diff, apply manifests.
    ///
    /// Every subcommand reads tonin.toml from `--path` (default: current
    /// dir) and either writes YAML offline (`generate`) or talks to the
    /// cluster (`validate` / `diff` / `apply`).
    K8s {
        #[command(subcommand)]
        cmd: commands::k8s::K8sCmd,
    },
    /// Protocol Buffers / gRPC codegen.
    ///
    /// Stub today — real codegen runs in the scaffolded service's
    /// `build.rs` via `tonin-build` → `tonic-build` (prost). This
    /// subcommand exists so the long-term `protoc-gen-micro` workflow
    /// has a stable invocation surface.
    Proto {
        #[command(subcommand)]
        cmd: commands::proto::ProtoCmd,
    },
    /// Service lifecycle: scaffold new, run locally.
    Service {
        #[command(subcommand)]
        cmd: commands::service::ServiceCmd,
    },
    /// Print a machine-readable manifest of every command, argument, and
    /// prerequisite. Intended for coding agents that need to plan against
    /// the CLI without parsing free-form `--help` output.
    Describe(commands::describe::DescribeArgs),
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        TopCmd::K8s { cmd } => commands::k8s::run(cmd),
        TopCmd::Proto { cmd } => commands::proto::run(cmd),
        TopCmd::Service { cmd } => commands::service::run(cmd),
        TopCmd::Describe(args) => commands::describe::run(args, &Cli::command()),
    }
}
