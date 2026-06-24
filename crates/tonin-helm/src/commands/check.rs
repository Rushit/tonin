//! `tonin-helm check` — verify that chart/ matches what `generate` would produce.
//!
//! Runs the exact same render pipeline as `generate`, but into a temp dir, then
//! diffs that output against the committed chart. Catches structural drift the
//! version-string check misses (new secret, new caller, changed replicas, …).
//!
//! Only files `generate` itself writes are compared — hand-authored templates
//! under `chart/templates/` (e.g. a custom CronJob) are never flagged as drift.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::generate::{self, GenerateArgs};

#[derive(clap::Args)]
pub struct CheckArgs {
    /// Path to the service directory containing `tonin.toml`.
    /// Defaults to `$TONIN_SERVICE_DIR` if set, otherwise the current directory.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// The committed chart directory to compare against.
    /// Defaults to `<path>/chart` (same target as `generate --out`).
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Environments to check `values-<env>.yaml` for.
    /// Defaults to: dev, staging, prod.
    #[arg(long = "env", value_name = "ENV")]
    pub envs: Vec<String>,
}

pub fn run(args: CheckArgs) -> Result<()> {
    let service_dir = generate::resolve_service_dir(args.path.as_deref())?;
    let chart_dir = args
        .out
        .clone()
        .unwrap_or_else(|| service_dir.join("chart"));

    let drift = collect_drift(&service_dir, &chart_dir, &args.envs)?;

    if drift.is_empty() {
        println!("✓ chart is up to date");
        return Ok(());
    }

    eprintln!("✗ chart/ is out of date — `generate` would change:");
    for line in &drift {
        eprintln!("    {line}");
    }
    eprintln!("\nRun: make helm-generate");
    std::process::exit(1);
}

/// Re-render the chart into a temp dir and return a sorted, human-readable list
/// of drifted files (relative path + status). Empty ⇒ chart is up to date.
///
/// Only files produced by `generate` are considered; files present in
/// `chart_dir` but not regenerated (user-authored templates) are ignored.
fn collect_drift(service_dir: &Path, chart_dir: &Path, envs: &[String]) -> Result<Vec<String>> {
    let tmp = tempfile::TempDir::new().context("creating temp dir for chart check")?;

    generate::run(GenerateArgs {
        path: Some(service_dir.to_path_buf()),
        out: Some(tmp.path().to_path_buf()),
        envs: envs.to_vec(),
    })
    .context("re-rendering chart for drift check")?;

    let mut drift = Vec::new();
    for rel in collect_files(tmp.path())? {
        let generated = tmp.path().join(&rel);
        let committed = chart_dir.join(&rel);
        let rel_disp = rel.display();
        match std::fs::read(&committed) {
            Err(_) => drift.push(format!("added    {rel_disp}")),
            Ok(committed_bytes) => {
                let generated_bytes = std::fs::read(&generated)
                    .with_context(|| format!("reading {}", generated.display()))?;
                if generated_bytes != committed_bytes {
                    drift.push(format!("changed  {rel_disp}"));
                }
            }
        }
    }
    drift.sort();
    Ok(drift)
}

/// Recursively collect every file under `dir`, returned as paths relative to
/// `dir`.
fn collect_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_into(dir, dir, &mut files)?;
    Ok(files)
}

fn collect_into(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_into(root, &path, files)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            files.push(rel.to_path_buf());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVC_TOML: &str = "\
schema = \"v1\"
[service]
name    = \"svc\"
version = \"0.1.0\"
type    = \"http\"
port    = 7001
[deploy]
namespace = \"demo\"
replicas  = 1
[resources]
cpu    = \"100m\"
memory = \"128Mi\"
";

    /// Generate a chart, then return (service_dir tempdir, chart_dir).
    fn generate_chart() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tonin.toml"), SVC_TOML).unwrap();
        let chart = dir.path().join("chart");
        generate::run(GenerateArgs {
            path: Some(dir.path().to_path_buf()),
            out: Some(chart.clone()),
            envs: vec!["dev".into()],
        })
        .unwrap();
        (dir, chart)
    }

    #[test]
    fn fresh_generate_has_no_drift() {
        let (dir, chart) = generate_chart();
        let drift = collect_drift(dir.path(), &chart, &["dev".to_string()]).unwrap();
        assert!(drift.is_empty(), "expected no drift, got {drift:?}");
    }

    #[test]
    fn mutated_file_is_detected() {
        let (dir, chart) = generate_chart();
        std::fs::write(chart.join("Chart.yaml"), "garbage: true\n").unwrap();
        let drift = collect_drift(dir.path(), &chart, &["dev".to_string()]).unwrap();
        assert!(
            drift.iter().any(|d| d.contains("Chart.yaml")),
            "expected Chart.yaml drift, got {drift:?}"
        );
    }

    #[test]
    fn user_authored_template_is_not_drift() {
        let (dir, chart) = generate_chart();
        // A file `generate` never writes must not be reported as drift.
        std::fs::write(
            chart.join("templates").join("custom-cronjob.yaml"),
            "kind: CronJob\n",
        )
        .unwrap();
        let drift = collect_drift(dir.path(), &chart, &["dev".to_string()]).unwrap();
        assert!(drift.is_empty(), "user file must not be drift: {drift:?}");
    }
}
