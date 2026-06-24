pub mod check;
pub mod generate;
pub mod proxy;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum HelmCmd {
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

    /// Raw passthrough to the `helm` binary: `tonin helm -- <helm args>`.
    ///
    /// Everything after `--` is forwarded verbatim to `helm`.
    /// Example: `tonin helm -- lint chart/`
    #[command(external_subcommand)]
    Raw(Vec<String>),
}

pub fn run(cmd: HelmCmd) -> Result<()> {
    match cmd {
        HelmCmd::Generate(a) => generate::run(a),
        HelmCmd::Check(a) => check::run(a),
        HelmCmd::Upgrade(a) => proxy::run_upgrade(a),
        HelmCmd::Template(a) => proxy::run_template(a),
        HelmCmd::Diff(a) => proxy::run_diff(a),
        HelmCmd::Status(a) => proxy::run_status(a),
        HelmCmd::Rollback(a) => proxy::run_rollback(a),
        HelmCmd::History(a) => proxy::run_history(a),
        HelmCmd::Uninstall(a) => proxy::run_uninstall(a),
        HelmCmd::Raw(args) => proxy::run_raw(args),
    }
}
