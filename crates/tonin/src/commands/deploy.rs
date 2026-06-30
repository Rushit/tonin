use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use tonin_plugin::deploy_backend::{
    DeployBackend, DryRunBackend, HelmBackend, NoopReporter, TelemetryReporter,
};
use tonin_plugin::lock::{GitLockStore, LockStore};
use tonin_plugin::{Plan, wave_groups};

#[derive(Args)]
pub struct DeployArgs {
    /// Target environment — reads `environments/<ENV>/lock.toml`.
    #[arg(long, env = "TONIN_ENV")]
    pub env: String,

    /// Force-reconcile ALL services even if already at the lock version.
    #[arg(long)]
    pub converge: bool,

    /// Only deploy services affected by git changes.
    #[arg(long)]
    pub affected: bool,

    /// Workspace root containing tonin.toml files + environments/.
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Helm upgrade timeout per service (e.g. "300s", "5m").
    #[arg(long, default_value = "300s")]
    pub timeout: String,

    /// Show what would change without applying. No helm calls are made.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: DeployArgs) -> Result<()> {
    let root = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let backend: Box<dyn DeployBackend> = if args.dry_run {
        Box::new(DryRunBackend)
    } else {
        Box::new(HelmBackend)
    };
    let reporter: Box<dyn TelemetryReporter> = Box::new(NoopReporter); // M3 wires real OTLP

    let store = GitLockStore::new(&root);
    let lock = store.read(&args.env)?;
    let plans = Plan::load_workspace_with_env(&root, &args.env)?;
    let waves = wave_groups(&plans);

    for (wave_num, wave_plans) in waves.iter().enumerate() {
        println!("▶ wave {wave_num}");
        for plan in wave_plans {
            let Some(svc_lock) = lock.services.get(&plan.name) else {
                println!("  {}: not in lock — skipping", plan.name);
                continue;
            };

            let running =
                backend.running_version(&format!("{}-{}", plan.name, args.env), &plan.namespace)?;

            if !args.converge && running.as_deref() == Some(&svc_lock.version) {
                println!(
                    "  {}: already at {} — skipping",
                    plan.name, svc_lock.version
                );
                continue;
            }

            println!(
                "  {}: {} → {}",
                plan.name,
                running.as_deref().unwrap_or("none"),
                svc_lock.version
            );

            if args.dry_run {
                continue;
            }

            let timeout_secs = parse_timeout(&args.timeout)?;
            let values = tonin_plugin::deploy_backend::DeployValues {
                image: svc_lock.image.clone(),
                version: svc_lock.version.clone(),
                release: format!("{}-{}", plan.name, args.env),
                namespace: plan.namespace.clone(),
                timeout: Duration::from_secs(timeout_secs),
            };
            backend.upgrade(&values)?;

            reporter.deploy_event(&tonin_plugin::deploy_backend::DeployEvent {
                service: plan.name.clone(),
                version: svc_lock.version.clone(),
                env: args.env.clone(),
                wave: wave_num,
                status: tonin_plugin::deploy_backend::DeployStatus::Success,
                git_sha: svc_lock.git_sha.clone(),
                image: svc_lock.image.clone(),
            });

            println!("  {} ✓", plan.name);
        }
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
