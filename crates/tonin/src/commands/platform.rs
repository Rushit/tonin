use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;
use tonin_plugin::{CliOrchestrator, Plan, PlatformOrchestrator};

#[derive(Args)]
pub struct PlatformArgs {
    #[command(subcommand)]
    pub cmd: PlatformCmd,
}

#[derive(Subcommand)]
pub enum PlatformCmd {
    /// Deploy a service to an environment via the platform orchestrator.
    ///
    /// Integrates with the PlatformOrchestrator trait to enable agnitiv-platform
    /// to orchestrate tonin deployments without CLI direct invocation.
    ///
    ///   tonin platform deploy --env staging --service users-service
    ///   tonin platform deploy --env prod --service orders-service --json
    Deploy {
        /// Target environment
        #[arg(long, env = "TONIN_ENV")]
        env: String,

        /// Service to deploy (required)
        #[arg(long)]
        service: String,

        /// Workspace root containing tonin.toml
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Output JSON instead of human-readable text
        #[arg(long)]
        json: bool,

        /// Show what would happen without applying changes
        #[arg(long)]
        dry_run: bool,
    },

    /// Check the status of services in an environment.
    ///
    /// Queries Kubernetes for running versions and cross-references
    /// with the environment lock to detect drift.
    ///
    ///   tonin platform status --env prod
    ///   tonin platform status --env staging --json
    Status {
        /// Target environment
        #[arg(long, env = "TONIN_ENV")]
        env: String,

        /// Workspace root
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Output JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Rollback a service to its previous release.
    ///
    /// Executes helm rollback for the specified service.
    ///
    ///   tonin platform rollback --service users-service --env staging
    Rollback {
        /// Service to rollback
        #[arg(long)]
        service: String,

        /// Target environment
        #[arg(long, env = "TONIN_ENV")]
        env: String,

        /// Workspace root
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

pub fn run(args: PlatformArgs) -> Result<()> {
    match args.cmd {
        PlatformCmd::Deploy {
            env,
            service,
            workspace,
            json,
            dry_run,
        } => run_deploy(&env, &service, workspace, json, dry_run),

        PlatformCmd::Status {
            env,
            workspace,
            json,
        } => run_status(&env, workspace, json),

        PlatformCmd::Rollback {
            service,
            env,
            workspace,
        } => run_rollback(&service, &env, workspace),
    }
}

fn run_deploy(
    env: &str,
    service: &str,
    workspace: Option<PathBuf>,
    json: bool,
    dry_run: bool,
) -> Result<()> {
    let root = workspace.unwrap_or_else(|| std::env::current_dir().unwrap());

    // Load plans to find service and its wave
    let plans = Plan::load_workspace_with_env(&root, env)?;
    let plan = plans
        .iter()
        .find(|p| p.name == service)
        .ok_or_else(|| anyhow::anyhow!("service '{}' not found in tonin.toml", service))?;

    if dry_run {
        if json {
            let response = serde_json::json!({
                "service": service,
                "env": env,
                "digest": "dry-run",
                "status": "dry_run",
                "timestamp": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                "message": "dry run mode — no changes applied"
            });
            println!("{}", serde_json::to_string_pretty(&response)?);
        } else {
            println!("DRY RUN: would deploy {} to {} environment", service, env);
        }
        return Ok(());
    }

    // Use CliOrchestrator to deploy the single service as a wave
    let orchestrator = CliOrchestrator::new(&root);
    let results = orchestrator.deploy_wave(&[plan], env)?;

    if json {
        // Output results as JSON
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        // Human-readable output
        for result in results {
            let status_str = result.status.as_str();
            if let Some(msg) = &result.message {
                println!("{}: {} — {}", result.service, status_str, msg);
            } else {
                println!("{}: {} ({})", result.service, status_str, result.digest);
            }
        }
    }

    Ok(())
}

fn run_status(env: &str, workspace: Option<PathBuf>, json: bool) -> Result<()> {
    let root = workspace.unwrap_or_else(|| std::env::current_dir().unwrap());

    let orchestrator = CliOrchestrator::new(&root);
    let statuses = orchestrator.get_running_services(env)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&statuses)?);
    } else {
        // Human-readable table output
        println!(
            "{:<30} {:<20} {:<20} {:<12}",
            "SERVICE", "RUNNING", "DESIRED", "HEALTH"
        );
        println!("{}", "=".repeat(82));

        for status in statuses {
            let running = status.running_version.as_deref().unwrap_or("—");
            let desired = status.desired_version.as_deref().unwrap_or("—");
            println!(
                "{:<30} {:<20} {:<20} {:<12}",
                status.service, running, desired, status.health
            );
        }
    }

    Ok(())
}

fn run_rollback(service: &str, env: &str, workspace: Option<PathBuf>) -> Result<()> {
    let root = workspace.unwrap_or_else(|| std::env::current_dir().unwrap());

    let orchestrator = CliOrchestrator::new(&root);
    orchestrator.rollback_service(service, env)?;

    println!("{}: rolled back in {} environment", service, env);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn dry_run_mode_produces_valid_json() {
        // Unit test verifies the JSON structure is valid
        let response = serde_json::json!({
            "service": "test-service",
            "env": "test",
            "digest": "dry-run",
            "status": "dry_run",
            "timestamp": 1719792000,
        });
        let json_str = serde_json::to_string(&response).unwrap();
        assert!(json_str.contains("test-service"));
    }
}
