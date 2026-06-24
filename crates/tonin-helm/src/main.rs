//! `tonin-helm` — Helm chart generation and lifecycle management for tonin services.
//!
//! Invoked either directly (`tonin-helm generate`) or via the tonin plugin
//! dispatch (`tonin helm generate`). When dispatched by tonin, the env vars
//! `TONIN_SERVICE_DIR` and `TONIN_CLI_VERSION` are set automatically.
//!
//! ## Usage
//!
//! ```text
//! tonin helm generate            # render chart/ from tonin.toml
//! tonin helm upgrade --env prod  # helm upgrade --install with context auto-resolved
//! tonin helm template            # helm template with context
//! tonin helm diff                # helm diff upgrade (requires helm-diff plugin)
//! tonin helm status              # helm status
//! tonin helm rollback            # helm rollback
//! tonin helm history             # helm history
//! tonin helm uninstall           # helm uninstall
//! tonin helm -- lint chart/      # raw passthrough to helm
//! ```

mod commands;

use anyhow::Result;
use clap::{Parser, Subcommand};
use commands::{check, generate, proxy};

#[derive(Parser)]
#[command(
    name = "tonin-helm",
    version,
    about = "Helm chart generation and lifecycle management for tonin services",
    long_about = "Reads tonin.toml and either generates a Helm chart (generate) or \
                  wraps helm commands with auto-resolved release name, namespace, \
                  and values files.

EXAMPLES

  Generate chart from tonin.toml:
    tonin helm generate
    tonin helm generate --out ./deploy/chart

  Deploy to a cluster:
    tonin helm upgrade --env prod --atomic --wait
    tonin helm diff    --env prod
    tonin helm status

  Raw helm passthrough (prefix with --):
    tonin helm -- lint chart/
    tonin helm -- get manifest identity -n agnitiv

ENVIRONMENT

  TONIN_SERVICE_DIR   Set by `tonin` dispatch; the service directory.
  TONIN_CLI_VERSION   Set by `tonin` dispatch; the calling CLI version.
  TONIN_ENV           Default environment overlay (overridden by --env)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a Helm chart from `tonin.toml`.
    ///
    /// Writes `chart/Chart.yaml`, `chart/values.yaml`,
    /// `chart/values-<env>.yaml` (one per env), and the generic
    /// `chart/templates/` Go template files.
    Generate(generate::GenerateArgs),

    /// Verify that the committed chart/ matches what `generate` would produce.
    /// Exits 1 with a diff summary if anything is stale.
    Check(check::CheckArgs),

    /// Deploy to a cluster: `helm upgrade --install` with context auto-resolved.
    Upgrade(proxy::UpgradeArgs),

    /// Render chart templates locally: `helm template` with context.
    Template(proxy::SimpleArgs),

    /// Show a diff against the live release (requires helm-diff plugin).
    Diff(proxy::SimpleArgs),

    /// Show the status of the live release.
    Status(proxy::SimpleArgs),

    /// Roll back the release to a previous revision.
    Rollback(proxy::SimpleArgs),

    /// Show the revision history of the release.
    History(proxy::SimpleArgs),

    /// Remove the release from the cluster.
    Uninstall(proxy::SimpleArgs),
}

/// Minimum tonin CLI this plugin needs. Sourced from `tonin-plugin` so the
/// compatibility floor tracks the schema the plugin actually compiles against.
const MIN_TONIN_CLI: &str = tonin_plugin::RECOMMENDED_CLI_MIN;

fn main() -> Result<()> {
    // tonin plugin list --verbose calls this flag to get a one-line description.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--tonin-describe") {
        println!("Helm chart generation and lifecycle management for tonin services");
        return Ok(());
    }

    // `tonin upgrade` / `tonin doctor` read this JSON to discover the plugin's
    // version, repo, and the minimum CLI it needs — no per-plugin knowledge in
    // the CLI. Keep the shape in sync with `plugin::PluginMeta` in the CLI.
    if args.get(1).map(String::as_str) == Some("--tonin-meta") {
        println!(
            r#"{{"name":"helm","version":"{}","min_tonin":"{}","repo":"Rushit/tonin-helm"}}"#,
            env!("CARGO_PKG_VERSION"),
            MIN_TONIN_CLI,
        );
        return Ok(());
    }

    // When dispatched by an out-of-date `tonin`, nudge the user to upgrade.
    warn_if_tonin_outdated();

    // `tonin helm -- <args>` arrives here as `["--", ...]` after tonin strips "helm".
    // clap can't parse `--` as a subcommand, so handle it before parsing.
    if args.get(1).map(String::as_str) == Some("--") {
        return proxy::run_raw(args[2..].to_vec());
    }

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Generate(a) => generate::run(a),
        Cmd::Check(a) => check::run(a),
        Cmd::Upgrade(a) => proxy::run_upgrade(a),
        Cmd::Template(a) => proxy::run_template(a),
        Cmd::Diff(a) => proxy::run_diff(a),
        Cmd::Status(a) => proxy::run_status(a),
        Cmd::Rollback(a) => proxy::run_rollback(a),
        Cmd::History(a) => proxy::run_history(a),
        Cmd::Uninstall(a) => proxy::run_uninstall(a),
    }
}

/// Warn (don't block) when the dispatching `tonin` CLI is older than this
/// plugin needs. `TONIN_CLI_VERSION` is set only when invoked via `tonin`
/// dispatch, so direct `tonin-helm` runs never warn. Silence with
/// `TONIN_NO_COMPAT_CHECK=1`.
fn warn_if_tonin_outdated() {
    if std::env::var_os("TONIN_NO_COMPAT_CHECK").is_some() {
        return;
    }
    let Ok(cli) = std::env::var("TONIN_CLI_VERSION") else {
        return;
    };
    if version_lt(&cli, MIN_TONIN_CLI) {
        eprintln!(
            "warning: tonin-helm {} needs tonin >= {} but you have {cli}.\n\
             \x20        Run `tonin upgrade` to update both.",
            env!("CARGO_PKG_VERSION"),
            MIN_TONIN_CLI,
        );
    }
}

/// `true` if semver `a` is strictly older than `b`. Unparseable versions
/// compare as "not older" so a malformed version never triggers a warning.
fn version_lt(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => x < y,
        _ => false,
    }
}

/// Parse `vX.Y.Z` / `X.Y.Z[-pre]` into a comparable `(major, minor, patch)`.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}
