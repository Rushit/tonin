//! Proxy commands that delegate to the `helm` binary with context auto-resolved
//! from `tonin.toml`.
//!
//! `exec()` on Unix (process replacement). On other platforms, spawn + forward
//! exit code.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use tonin_plugin::Plan;

use super::generate::resolve_service_dir;

/// Shared flags present on every proxy subcommand.
#[derive(clap::Args, Clone)]
pub struct CommonArgs {
    /// Service directory containing `tonin.toml`.
    /// Defaults to `$TONIN_SERVICE_DIR`, then the current directory.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// Overlay environment to use for namespace / replica / values resolution.
    /// Defaults to `$TONIN_ENV`, then `"dev"`.
    #[arg(long)]
    pub env: Option<String>,

    /// Chart directory. Defaults to `<path>/chart`.
    #[arg(long)]
    pub chart: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct UpgradeArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Pass `--atomic` to helm upgrade (roll back on failure).
    #[arg(long)]
    pub atomic: bool,
    /// Pass `--wait` to helm upgrade.
    #[arg(long)]
    pub wait: bool,
    /// Timeout string forwarded to helm (e.g. `10m0s`).
    #[arg(long, default_value = "10m0s")]
    pub timeout: String,
    /// Extra flags forwarded verbatim to helm.
    #[arg(last = true)]
    pub extra: Vec<String>,
}

#[derive(clap::Args)]
pub struct SimpleArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Extra flags forwarded verbatim to helm.
    #[arg(last = true)]
    pub extra: Vec<String>,
}

// ---------- run functions ----------

pub fn run_upgrade(args: UpgradeArgs) -> Result<()> {
    let (plan, env, chart_dir) = resolve_context(&args.common)?;

    let mut cmd = helm();
    cmd.args(["upgrade", "--install", &plan.name])
        .arg(&chart_dir)
        .args(["-f", &values_file(&chart_dir, "values.yaml")])
        .args([
            "-f",
            &values_file(&chart_dir, &format!("values-{env}.yaml")),
        ])
        .args(["--namespace", &plan.namespace])
        .arg("--create-namespace");

    if args.atomic {
        cmd.arg("--atomic");
    }
    if args.wait {
        cmd.arg("--wait");
    }
    cmd.args(["--timeout", &args.timeout]);
    cmd.args(&args.extra);

    exec_helm(cmd)
}

pub fn run_template(args: SimpleArgs) -> Result<()> {
    let (plan, env, chart_dir) = resolve_context(&args.common)?;

    let mut cmd = helm();
    cmd.args(["template", &plan.name])
        .arg(&chart_dir)
        .args(["-f", &values_file(&chart_dir, "values.yaml")])
        .args([
            "-f",
            &values_file(&chart_dir, &format!("values-{env}.yaml")),
        ])
        .args(["--namespace", &plan.namespace])
        .args(&args.extra);

    exec_helm(cmd)
}

pub fn run_diff(args: SimpleArgs) -> Result<()> {
    let (plan, env, chart_dir) = resolve_context(&args.common)?;

    let mut cmd = helm();
    // helm-diff plugin: `helm diff upgrade`
    cmd.args(["diff", "upgrade", &plan.name])
        .arg(&chart_dir)
        .args(["-f", &values_file(&chart_dir, "values.yaml")])
        .args([
            "-f",
            &values_file(&chart_dir, &format!("values-{env}.yaml")),
        ])
        .args(["--namespace", &plan.namespace])
        .args(&args.extra);

    exec_helm(cmd)
}

pub fn run_status(args: SimpleArgs) -> Result<()> {
    let (plan, env, _chart_dir) = resolve_context(&args.common)?;
    let mut cmd = helm();
    cmd.args(["status", &plan.name])
        .args(["--namespace", &plan.namespace])
        .args(&args.extra);
    let _ = env;
    exec_helm(cmd)
}

pub fn run_rollback(args: SimpleArgs) -> Result<()> {
    let (plan, env, _chart_dir) = resolve_context(&args.common)?;
    let mut cmd = helm();
    cmd.args(["rollback", &plan.name])
        .args(["--namespace", &plan.namespace])
        .args(&args.extra);
    let _ = env;
    exec_helm(cmd)
}

pub fn run_uninstall(args: SimpleArgs) -> Result<()> {
    let (plan, env, _chart_dir) = resolve_context(&args.common)?;
    let mut cmd = helm();
    cmd.args(["uninstall", &plan.name])
        .args(["--namespace", &plan.namespace])
        .args(&args.extra);
    let _ = env;
    exec_helm(cmd)
}

pub fn run_history(args: SimpleArgs) -> Result<()> {
    let (plan, env, _chart_dir) = resolve_context(&args.common)?;
    let mut cmd = helm();
    cmd.args(["history", &plan.name])
        .args(["--namespace", &plan.namespace])
        .args(&args.extra);
    let _ = env;
    exec_helm(cmd)
}

/// Raw passthrough: `tonin helm -- <anything>` → `helm <anything>`.
pub fn run_raw(extra: Vec<String>) -> Result<()> {
    let mut cmd = helm();
    cmd.args(&extra);
    exec_helm(cmd)
}

// ---------- helpers ----------

fn resolve_context(common: &CommonArgs) -> Result<(Plan, String, PathBuf)> {
    let service_dir = resolve_service_dir(common.path.as_deref())?;
    let env = tonin_plugin::select_env(common.env.as_deref());
    let plan = Plan::load_with_env(&service_dir.join("tonin.toml"), &env)
        .with_context(|| format!("loading tonin.toml in {}", service_dir.display()))?;
    let chart_dir = common
        .chart
        .clone()
        .unwrap_or_else(|| service_dir.join("chart"));
    Ok((plan, env, chart_dir))
}

fn helm() -> Command {
    Command::new("helm")
}

fn values_file(chart_dir: &Path, name: &str) -> String {
    chart_dir.join(name).to_string_lossy().into_owned()
}

#[cfg(unix)]
fn exec_helm(mut cmd: Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = cmd.exec();
    bail!("failed to exec helm: {err}")
}

#[cfg(not(unix))]
fn exec_helm(mut cmd: Command) -> Result<()> {
    let status = cmd.status().context("failed to spawn helm")?;
    std::process::exit(status.code().unwrap_or(1));
}
