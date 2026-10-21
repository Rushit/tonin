use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use tonin_plugin::deploy_backend::{
    DeployBackend, DeployValues, DryRunBackend, HelmBackend, NoopReporter, TelemetryReporter,
};
use tonin_plugin::lock::{EnvLock, GitLockStore, LockStore};
use tonin_plugin::{Plan, wave_groups};

use super::affected::{self, Granularity};

#[derive(Args)]
pub struct DeployArgs {
    /// Target environment — reads `environments/<ENV>/lock.toml` and renders
    /// each chart with `values.yaml` + `values-<ENV>.yaml`.
    #[arg(long, env = "TONIN_ENV")]
    pub env: String,

    /// Force-reconcile ALL selected services even if already at the lock image.
    #[arg(long)]
    pub converge: bool,

    /// Only deploy services affected by git changes since `--base`
    /// (the same computation as `tonin affected`).
    #[arg(long, requires = "base")]
    pub affected: bool,

    /// Git ref to diff HEAD against for `--affected`.
    #[arg(long, value_name = "REF", requires = "affected")]
    pub base: Option<String>,

    /// Only deploy this service (repeat for several). Combined with
    /// `--affected`, deploys the services that match both.
    #[arg(long = "service", value_name = "NAME")]
    pub services: Vec<String>,

    /// Workspace root containing tonin.toml files + environments/.
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Helm upgrade timeout per service (e.g. "300s", "5m").
    #[arg(long, default_value = "300s")]
    pub timeout: String,

    /// Show what would change without applying. No helm calls are made; the
    /// helm command for each service is printed instead.
    #[arg(long)]
    pub dry_run: bool,
}

/// One service upgrade, in wave order.
struct Step {
    wave: usize,
    values: DeployValues,
    git_sha: String,
}

pub fn run(args: DeployArgs) -> Result<()> {
    let root = match &args.workspace {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().context("cannot determine current directory")?,
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("workspace {}", root.display()))?;

    let backend: Box<dyn DeployBackend> = if args.dry_run {
        Box::new(DryRunBackend)
    } else {
        Box::new(HelmBackend)
    };
    let reporter: Box<dyn TelemetryReporter> = Box::new(NoopReporter); // M3 wires real OTLP

    let lock = GitLockStore::new(&root).read(&args.env)?;
    let plans = Plan::load_workspace_with_env(&root, &args.env)?;

    let affected = match (&args.affected, &args.base) {
        (true, Some(base)) => {
            let changed = affected::git_diff(base, "HEAD", &root)?;
            let result = affected::compute_affected(
                changed,
                &plans,
                Granularity::Service,
                base.clone(),
                "HEAD".into(),
            );
            Some(
                result
                    .affected_services
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| s.service)
                    .collect(),
            )
        }
        _ => None,
    };
    let selected = select_services(&plans, &args.services, affected)?;
    let timeout = Duration::from_secs(parse_timeout(&args.timeout)?);
    let steps = plan_steps(&plans, &lock, &args.env, selected.as_ref(), timeout)?;

    if steps.is_empty() {
        println!("nothing to deploy");
        return Ok(());
    }

    let mut current_wave = None;
    for step in steps {
        if current_wave != Some(step.wave) {
            println!("▶ wave {}", step.wave);
            current_wave = Some(step.wave);
        }
        let v = &step.values;
        let desired = v.image_ref().to_string();
        let running = backend.running_image(&v.release, &v.namespace)?;

        if !args.converge && in_sync(running.as_deref(), v) {
            println!("  {}: already at {desired} — skipping", v.release);
            continue;
        }

        println!(
            "  {}: {} → {desired} ({})",
            v.release,
            running.as_deref().unwrap_or("none"),
            v.version
        );

        backend.upgrade(v)?;
        if args.dry_run {
            continue;
        }

        reporter.deploy_event(&tonin_plugin::deploy_backend::DeployEvent {
            service: v.release.clone(),
            version: v.version.clone(),
            env: args.env.clone(),
            wave: step.wave,
            status: tonin_plugin::deploy_backend::DeployStatus::Success,
            git_sha: step.git_sha.clone(),
            image: v.image.clone(),
        });

        println!("  {} ✓", v.release);
    }
    Ok(())
}

/// Whether a release running `running` (the image its values render, from
/// [`DeployBackend::running_image`]) already runs the lock's image.
fn in_sync(running: Option<&str>, values: &DeployValues) -> bool {
    values.is_running(running)
}

/// The services to deploy: the `--service` names (validated against the
/// workspace), intersected with the affected set when `--affected` is given.
/// `None` means every service.
fn select_services(
    plans: &[Plan],
    services: &[String],
    affected: Option<BTreeSet<String>>,
) -> Result<Option<BTreeSet<String>>> {
    for name in services {
        anyhow::ensure!(
            plans.iter().any(|p| &p.name == name),
            "--service {name}: no tonin.toml in the workspace declares that service"
        );
    }
    let named: Option<BTreeSet<String>> =
        (!services.is_empty()).then(|| services.iter().cloned().collect());
    Ok(match (named, affected) {
        (Some(named), Some(affected)) => Some(named.intersection(&affected).cloned().collect()),
        (named, affected) => named.or(affected),
    })
}

/// Build the upgrade for every selected service that is in the lock, in wave
/// order (waves are computed over the whole workspace).
fn plan_steps(
    plans: &[Plan],
    lock: &EnvLock,
    env: &str,
    selected: Option<&BTreeSet<String>>,
    timeout: Duration,
) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    for (wave, wave_plans) in wave_groups(plans).iter().enumerate() {
        for plan in wave_plans {
            if selected.is_some_and(|s| !s.contains(&plan.name)) {
                continue;
            }
            let Some(svc_lock) = lock.services.get(&plan.name) else {
                println!("  {}: not in lock — skipping", plan.name);
                continue;
            };
            let values = DeployValues::for_plan(
                plan,
                env,
                svc_lock.image.clone(),
                svc_lock.version.clone(),
                timeout,
            );
            ensure_chart(&values, &plan.name, env)?;
            steps.push(Step {
                wave,
                values,
                git_sha: svc_lock.git_sha.clone(),
            });
        }
    }
    Ok(steps)
}

/// Fail early (before any upgrade) when a chart or values file is missing.
fn ensure_chart(values: &DeployValues, service: &str, env: &str) -> Result<()> {
    let required =
        std::iter::once(values.chart.join("Chart.yaml")).chain(values.values_files.iter().cloned());
    for file in required {
        anyhow::ensure!(
            Path::new(&file).is_file(),
            "{service}: {} is missing; run `tonin helm generate --env {env}` for this service",
            file.display()
        );
    }
    Ok(())
}

fn parse_timeout(s: &str) -> Result<u64> {
    if let Some(n) = s.strip_suffix('s') {
        return Ok(n.parse()?);
    }
    if let Some(n) = s.strip_suffix('m') {
        return Ok(n.parse::<u64>()? * 60);
    }
    Ok(s.parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonin_plugin::lock::ServiceLock;

    const DIGEST: &str = "sha256:0123456789abcdef";

    /// A workspace with `identity` (wave 0) and `api` (depends on identity,
    /// wave 1), both with generated-looking charts, plus a prd lock.
    fn workspace() -> (tempfile::TempDir, Vec<Plan>, EnvLock) {
        let ws = tempfile::tempdir().unwrap();
        for (name, deps) in [
            ("identity", ""),
            ("api", "[depends_on]\nidentity = \"plat-{env}\"\n"),
        ] {
            let dir = ws.path().join(name);
            std::fs::create_dir_all(dir.join("chart")).unwrap();
            std::fs::write(
                dir.join("tonin.toml"),
                format!(
                    "schema = \"v1\"\n[service]\nname = \"{name}\"\nversion = \"0.1.0\"\n\
                     [deploy]\nnamespace = \"plat-{{env}}\"\nreplicas = 1\n\
                     [resources]\ncpu = \"100m\"\nmemory = \"128Mi\"\n{deps}"
                ),
            )
            .unwrap();
            for f in ["Chart.yaml", "values.yaml", "values-prd.yaml"] {
                std::fs::write(dir.join("chart").join(f), "").unwrap();
            }
        }
        let root = ws.path().canonicalize().unwrap();
        let plans = Plan::load_workspace_with_env(&root, "prd").unwrap();
        let mut lock = EnvLock {
            schema: "v1".into(),
            env: "prd".into(),
            services: Default::default(),
        };
        for name in ["identity", "api"] {
            lock.services.insert(
                name.into(),
                ServiceLock {
                    version: "1.2.0".into(),
                    image: format!("ghcr.io/org/{name}@{DIGEST}"),
                    git_sha: "deadbeef".into(),
                },
            );
        }
        (ws, plans, lock)
    }

    fn steps(plans: &[Plan], lock: &EnvLock, selected: Option<&[&str]>) -> Vec<Step> {
        let selected: Option<BTreeSet<String>> =
            selected.map(|s| s.iter().map(|n| n.to_string()).collect());
        plan_steps(
            plans,
            lock,
            "prd",
            selected.as_ref(),
            Duration::from_secs(60),
        )
        .unwrap()
    }

    #[test]
    fn upgrade_uses_absolute_chart_both_values_files_and_lock_digest() {
        let (ws, plans, lock) = workspace();
        let steps = steps(&plans, &lock, None);
        let order: Vec<_> = steps
            .iter()
            .map(|s| (s.wave, s.values.release.as_str()))
            .collect();
        assert_eq!(order, [(0, "identity"), (1, "api")]);

        let chart = ws.path().canonicalize().unwrap().join("identity/chart");
        let args = steps[0].values.helm_upgrade_args();
        let pos = |a: &str| args.iter().position(|x| x == a).unwrap();
        // Release name is the service name — the same release `tonin helm upgrade` manages.
        assert_eq!(
            args[..4],
            ["upgrade", "--install", "identity", &chart.to_string_lossy()]
        );
        assert!(chart.is_absolute());
        assert_eq!(
            args[pos("-f") + 1],
            chart.join("values.yaml").to_string_lossy()
        );
        assert!(args.contains(&chart.join("values-prd.yaml").to_string_lossy().into_owned()));
        assert_eq!(args[pos("--namespace") + 1], "plat-prd");
        assert!(args.contains(&"image.repository=ghcr.io/org/identity".to_string()));
        assert!(args.contains(&format!("image.digest={DIGEST}")));
        assert!(
            !args.iter().any(|a| a.starts_with("image.tag=")),
            "{args:?}"
        );
    }

    #[test]
    fn service_filter_keeps_only_named_services() {
        let (_ws, plans, lock) = workspace();
        let selected = select_services(&plans, &["api".into()], None).unwrap();
        let steps = plan_steps(
            &plans,
            &lock,
            "prd",
            selected.as_ref(),
            Duration::from_secs(1),
        )
        .unwrap();
        let names: Vec<_> = steps.iter().map(|s| s.values.release.as_str()).collect();
        assert_eq!(names, ["api"]);
        assert_eq!(
            steps[0].wave, 1,
            "wave numbers come from the whole workspace"
        );
    }

    #[test]
    fn unknown_service_is_an_error() {
        let (_ws, plans, _lock) = workspace();
        let err = select_services(&plans, &["nope".into()], None).unwrap_err();
        assert!(err.to_string().contains("--service nope"), "{err}");
    }

    #[test]
    fn affected_and_service_filters_intersect() {
        let (_ws, plans, _lock) = workspace();
        let affected: BTreeSet<String> = ["identity".to_string()].into();
        let both = select_services(&plans, &["api".into()], Some(affected.clone())).unwrap();
        assert_eq!(both, Some(BTreeSet::new()));
        let only_affected = select_services(&plans, &[], Some(affected.clone())).unwrap();
        assert_eq!(only_affected, Some(affected));
        assert_eq!(select_services(&plans, &[], None).unwrap(), None);
    }

    #[test]
    fn missing_env_values_file_fails_before_any_upgrade() {
        let (ws, plans, lock) = workspace();
        std::fs::remove_file(ws.path().join("api/chart/values-prd.yaml")).unwrap();
        let err = plan_steps(&plans, &lock, "prd", None, Duration::from_secs(1))
            .err()
            .unwrap();
        assert!(err.to_string().contains("values-prd.yaml"), "{err}");
        assert!(
            err.to_string().contains("tonin helm generate --env prd"),
            "{err}"
        );
    }

    #[test]
    fn in_sync_compares_images_not_chart_versions() {
        let (_ws, plans, lock) = workspace();
        let api = &steps(&plans, &lock, Some(&["api"]))[0].values;
        assert!(in_sync(Some(&format!("ghcr.io/org/api@{DIGEST}")), api));
        // Same version, other digest: drift.
        assert!(!in_sync(Some("ghcr.io/org/api@sha256:ffff"), api));
        // A tag-based release (e.g. installed by `tonin helm upgrade`) is not
        // the pinned image, even at the same version.
        assert!(!in_sync(Some("ghcr.io/org/api:1.2.0"), api));
        assert!(!in_sync(None, api));
    }

    #[test]
    fn affected_requires_base() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            deploy: DeployArgs,
        }
        assert!(Cli::try_parse_from(["t", "--env", "prd", "--affected"]).is_err());
        assert!(Cli::try_parse_from(["t", "--env", "prd", "--base", "main"]).is_err());
        let cli = Cli::try_parse_from([
            "t",
            "--env",
            "prd",
            "--affected",
            "--base",
            "v1",
            "--service",
            "a",
            "--service",
            "b",
        ])
        .unwrap();
        assert_eq!(cli.deploy.services, ["a", "b"]);
    }
}
