use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use tonin_plugin::Plan;
use tonin_plugin::deploy_backend::{DeployBackend, HelmBackend, ImageRef};
use tonin_plugin::lock::{GitLockStore, LockStore};

#[derive(Args)]
pub struct StatusArgs {
    /// Target environment.
    #[arg(long, env = "TONIN_ENV")]
    pub env: String,

    /// Workspace root containing tonin.toml files + environments/.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub fn run(args: StatusArgs) -> Result<()> {
    let root = match args.workspace {
        Some(dir) => dir,
        None => std::env::current_dir().context("cannot determine current directory")?,
    };
    let store = GitLockStore::new(&root);
    let lock = store.read(&args.env)?;
    let plans = Plan::load_workspace_with_env(&root, &args.env)?;
    let backend = HelmBackend;

    println!("Environment: {}", args.env);
    println!("{:<25} {:<12} STATUS", "SERVICE", "LOCK");
    println!("{}", "─".repeat(65));

    let mut services: Vec<_> = lock.services.iter().collect();
    services.sort_by_key(|(name, _)| name.as_str());

    for (name, desired) in services {
        // Release = service name, in the namespace tonin.toml resolves for env.
        let status = match plans.iter().find(|p| &p.name == name) {
            None => "✗ NO tonin.toml".to_string(),
            Some(plan) => {
                let want = ImageRef::parse(&desired.image).to_string();
                match backend.running_image(name, &plan.namespace).unwrap_or(None) {
                    Some(running) if ImageRef::parse(&running).to_string() == want => {
                        "✓ in sync".to_string()
                    }
                    Some(running) => format!("✗ DRIFT (running {running})"),
                    None => "✗ NOT DEPLOYED".to_string(),
                }
            }
        };
        println!("{:<25} {:<12} {status}", name, desired.version);
    }
    Ok(())
}
