//! The `tonin` CLI — scaffold services, generate code from `.proto`, and
//! deploy with Helm (`tonin helm`).
//!
//! Subcommand groups dispatched from [`TopCmd`]:
//!
//! - `tonin service new <name>` — scaffold a Rust / Python / TypeScript
//!   service from the templates baked into this binary.
//! - `tonin proto generate` — `.proto` codegen entry point.
//! - `tonin helm generate / upgrade / diff / …` — Helm chart generation and
//!   lifecycle management (built-in; no separate install required).
//! - `tonin plugin` — list installed plugins discovered on `$PATH`.
//!
//! Any unrecognised subcommand triggers plugin dispatch: tonin looks for
//! `tonin-<name>` on `$PATH` and `exec()`s it with the remaining arguments
//! plus `TONIN_SERVICE_DIR` and `TONIN_CLI_VERSION` env vars set.
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
    long_about = "The tonin CLI scaffolds gRPC microservices and manages their lifecycle.

COMMON WORKFLOWS

  Scaffold and run locally:
    tonin service new greeter        # creates ./greeter/ with proto + impl
    cd greeter
    cargo run -p greeter               # boots gRPC + MCP locally

  Render Helm chart and deploy (built-in, no separate install needed):
    tonin helm generate              # writes ./chart/ from tonin.toml
    tonin helm diff --env prod       # show what would change
    tonin helm upgrade --env prod    # helm upgrade --install

PLUGINS

  tonin dispatches any unknown subcommand to a `tonin-<name>` binary on
  $PATH. Install a plugin with `cargo install tonin-<name>`, then use it
  as a first-class subcommand:

    tonin myplugin <args>            # delegates to tonin-myplugin on PATH

  List installed plugins:
    tonin plugin list
    tonin plugin list --verbose      # includes per-plugin descriptions

INTROSPECTION FOR CODING AGENTS

  Every subcommand supports `--help`. For a machine-readable manifest of
  every command + arg + prerequisite, use:

    tonin describe                   # text
    tonin describe --format json     # JSON

ENVIRONMENT

  TONIN_ENV         Default overlay env for Helm rendering (overlaid by --env).
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
    /// List and inspect installed tonin plugins.
    ///
    /// Plugins are executables named `tonin-<name>` anywhere on `$PATH`.
    /// Install them with `cargo install tonin-<name>` and invoke them as
    /// first-class subcommands: `tonin <name> [args...]`.
    Plugin {
        #[command(subcommand)]
        cmd: commands::plugin::PluginCmd,
    },
    /// Upgrade the tonin CLI and every installed plugin to the latest release.
    ///
    /// Downloads the canonical install script and runs it with one
    /// `--plugin` flag per plugin discovered on `$PATH` (each plugin reports
    /// its own repo via `--tonin-meta`). The full plan is shown and confirmed
    /// before anything changes — `--yes` skips the prompt, `--check` previews.
    Upgrade(commands::upgrade::UpgradeArgs),
    /// Check installed plugins for version compatibility with this CLI.
    ///
    /// Reports any plugin that needs a newer `tonin` and offers to run
    /// `tonin upgrade`. Local only — no network access.
    Doctor(commands::doctor::DoctorArgs),

    /// Helm chart generation and lifecycle management for tonin services.
    ///
    /// Reads `tonin.toml` and either generates a complete Helm chart
    /// (`helm generate`) or wraps helm commands with auto-resolved release
    /// name, namespace, and values files.
    ///
    ///   tonin helm generate            # render chart/ from tonin.toml
    ///   tonin helm upgrade --env prod  # helm upgrade --install
    ///   tonin helm diff    --env prod  # helm diff upgrade (requires helm-diff)
    ///   tonin helm -- lint chart/      # raw passthrough to helm
    Helm {
        #[command(subcommand)]
        cmd: commands::helm::HelmCmd,
    },

    /// Compute which services are affected by changed files, with wave ordering.
    Affected(commands::affected::AffectedArgs),

    /// Reconcile the cluster to the environment version lock in wave order.
    ///
    /// Reads environments/<env>/lock.toml, computes which services need updating,
    /// and runs helm upgrade in dependency-wave order. Use --dry-run to preview.
    ///
    ///   tonin deploy --env staging           # reconcile staging
    ///   tonin deploy --env staging --dry-run # preview only
    ///   tonin deploy --env prod --converge   # force all services
    Deploy(commands::deploy::DeployArgs),

    /// Show current fleet state vs the environment lock.
    ///
    /// Compares environments/<env>/lock.toml (desired) against running helm
    /// release versions (actual) and reports drift.
    ///
    ///   tonin status --env staging
    Status(commands::status::StatusArgs),

    /// Write a new service version into an environment lock.
    ///
    /// Called from CI after building and pushing a new image. Updates
    /// environments/<env>/lock.toml so tonin deploy can pick it up.
    ///
    ///   tonin release --service users-service --version 1.4.0 \
    ///                 --image ghcr.io/org/users-service@sha256:abc \
    ///                 --git-sha deadbeef --env staging
    Release(commands::release::ReleaseArgs),

    /// Run integration and end-to-end tests against a live environment.
    ///
    ///   tonin test e2e --env dev     # baggage propagation E2E test
    Test(commands::run_tests::TestArgs),

    /// [removed] Kubernetes manifest generation has moved to `tonin helm`.
    ///
    /// Install tonin-helm and use:
    ///   tonin helm generate    — render Helm chart from tonin.toml
    ///   tonin helm diff        — show pending cluster changes
    ///   tonin helm upgrade     — deploy / upgrade
    #[command(hide = true)]
    K8s {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    match Cli::try_parse() {
        Ok(cli) => dispatch(cli),
        Err(err) => {
            // If clap couldn't match a subcommand, check whether a plugin
            // binary handles it before surfacing the error.
            if err.kind() == clap::error::ErrorKind::InvalidSubcommand
                && let Some(name) = first_positional_arg()
            {
                return try_exec_plugin(&name);
            }
            err.exit()
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.cmd {
        TopCmd::Proto { cmd } => commands::proto::run(cmd),
        TopCmd::Service { cmd } => commands::service::run(cmd),
        TopCmd::Describe(args) => commands::describe::run(args, &Cli::command()),
        TopCmd::Plugin { cmd } => commands::plugin::run(cmd),
        TopCmd::Upgrade(args) => commands::upgrade::run(args),
        TopCmd::Doctor(args) => commands::doctor::run(args),
        TopCmd::Helm { cmd } => commands::helm::run(cmd),
        TopCmd::Affected(args) => commands::affected::run(args),
        TopCmd::Deploy(args) => commands::deploy::run(args),
        TopCmd::Status(args) => commands::status::run(args),
        TopCmd::Release(args) => commands::release::run(args),
        TopCmd::Test(args) => commands::run_tests::run(args),
        TopCmd::K8s { .. } => {
            eprintln!("error: `tonin k8s` has been removed.");
            eprintln!();
            eprintln!("  Kubernetes manifest generation is now `tonin helm` (built-in):");
            eprintln!("    tonin helm generate              # was: tonin k8s generate");
            eprintln!("    tonin helm diff --env prod       # was: tonin k8s diff");
            eprintln!("    tonin helm upgrade --env prod    # was: tonin k8s apply");
            std::process::exit(1);
        }
    }
}

/// Return the first non-flag argument after the binary name, which is the
/// subcommand / plugin name the user typed.
fn first_positional_arg() -> Option<String> {
    std::env::args().skip(1).find(|a| !a.starts_with('-'))
}

/// Look for `tonin-<name>` on $PATH and exec it, or print an install hint.
fn try_exec_plugin(name: &str) -> Result<()> {
    match commands::plugin::find_plugin(name) {
        Some(bin) => exec_plugin(bin, name),
        None => {
            eprintln!("error: unknown subcommand '{name}'");
            eprintln!("  hint: install `tonin-{name}` to use it as a plugin, e.g.:");
            eprintln!("        cargo install tonin-{name}");
            std::process::exit(1);
        }
    }
}

/// Replace the current process with the plugin binary (Unix `exec`).
/// All remaining args after the subcommand name are forwarded verbatim.
#[cfg(unix)]
fn exec_plugin(bin: std::path::PathBuf, name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;

    // Skip the binary name ("tonin") and the subcommand name (e.g. "helm").
    // Everything else is forwarded to the plugin as-is.
    let plugin_args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
    let cwd = std::env::current_dir()?;

    let err = std::process::Command::new(&bin)
        .args(&plugin_args)
        .env("TONIN_SERVICE_DIR", &cwd)
        .env("TONIN_CLI_VERSION", env!("CARGO_PKG_VERSION"))
        .exec(); // replaces the current process; only returns on error

    anyhow::bail!("failed to exec tonin-{name} ({}): {err}", bin.display())
}

#[cfg(not(unix))]
fn exec_plugin(bin: std::path::PathBuf, name: &str) -> Result<()> {
    // On non-Unix platforms spawn a child and forward its exit code.
    let plugin_args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
    let cwd = std::env::current_dir()?;

    let status = std::process::Command::new(&bin)
        .args(&plugin_args)
        .env("TONIN_SERVICE_DIR", &cwd)
        .env("TONIN_CLI_VERSION", env!("CARGO_PKG_VERSION"))
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn tonin-{name} ({}): {e}", bin.display()))?;

    std::process::exit(status.code().unwrap_or(1));
}
