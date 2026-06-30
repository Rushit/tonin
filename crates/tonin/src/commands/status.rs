use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use tonin_plugin::deploy_backend::{DeployBackend, HelmBackend};
use tonin_plugin::lock::{GitLockStore, LockStore};

#[derive(Args)]
pub struct StatusArgs {
    /// Target environment.
    #[arg(long, env = "TONIN_ENV")]
    pub env: String,

    /// Workspace root containing environments/.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub fn run(args: StatusArgs) -> Result<()> {
    let root = args
        .workspace
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let store = GitLockStore::new(&root);
    let lock = store.read(&args.env)?;
    let backend = HelmBackend;

    println!("Environment: {}", args.env);
    println!("{:<25} {:<12} {:<12} STATUS", "SERVICE", "LOCK", "RUNNING");
    println!("{}", "─".repeat(65));

    let mut services: Vec<_> = lock.services.iter().collect();
    services.sort_by_key(|(name, _)| name.as_str());

    for (name, desired) in services {
        let release = format!("{name}-{}", args.env);
        let running = backend.running_version(&release, &args.env).unwrap_or(None);
        let (status_icon, status_text) = match running.as_deref() {
            Some(v) if v == desired.version => ("✓", "in sync"),
            Some(_) => ("✗", "DRIFT"),
            None => ("✗", "NOT DEPLOYED"),
        };
        println!(
            "{:<25} {:<12} {:<12} {} {}",
            name,
            desired.version,
            running.as_deref().unwrap_or("—"),
            status_icon,
            status_text
        );
    }
    Ok(())
}
