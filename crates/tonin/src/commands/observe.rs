use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use tonin_plugin::{
    DockerComposeBackend, LogsValues, ObserveBackend, ObserveDryRunBackend, Plan, PortForwardValues,
};

/// Observe running services: stream logs and port-forward.
///
/// Post-deployment observation tools for debugging and testing running services.
/// Works with services started via `tonin run` or deployed to local Kubernetes.
#[derive(Args)]
pub struct ObserveArgs {
    /// Workspace root containing tonin.toml files.
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Show what would happen without actually running.
    #[arg(long)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub cmd: ObserveCmd,
}

#[derive(Subcommand)]
pub enum ObserveCmd {
    /// Stream logs from a running service.
    ///
    /// Connects to the running service container (docker or kubectl) and
    /// streams its logs. Use --follow to stream continuously.
    ///
    ///   tonin observe logs users-service            # last 10 lines
    ///   tonin observe logs users-service --follow   # stream continuously
    ///   tonin observe logs orders-service --tail 50 # last 50 lines
    Logs {
        /// Service name (e.g. "users-service")
        service: String,

        /// Stream continuously instead of exiting after showing history.
        #[arg(long)]
        follow: bool,

        /// Number of lines to show (default: 10).
        #[arg(long, default_value = "10")]
        tail: usize,

        /// Include timestamp for each log line.
        #[arg(long)]
        timestamp: bool,
    },

    /// Forward a local port to a running service's port.
    ///
    /// Establishes a tunnel from localhost to the service's remote port.
    /// Useful for connecting local tools (psql, redis-cli, etc.) to remote services.
    ///
    ///   tonin observe port-forward users-service --local 8001 --remote 50051
    ///   # then: nc localhost 8001 to reach users-service:50051
    PortForward {
        /// Service name (e.g. "orders-service")
        service: String,

        /// Local port to bind to (default: same as remote).
        #[arg(long)]
        local: Option<u16>,

        /// Remote service port to forward to (required).
        ///
        /// Inferred from tonin.toml \[service\].port if not specified,
        /// or use explicit --remote to override.
        #[arg(long)]
        remote: Option<u16>,
    },
}

pub fn run(args: ObserveArgs) -> Result<()> {
    let root = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    // Choose backend based on --dry-run and environment
    let backend: Box<dyn ObserveBackend> = if args.dry_run {
        Box::new(ObserveDryRunBackend)
    } else {
        // Default: Docker Compose backend (can later detect env and pick kubectl)
        Box::new(DockerComposeBackend)
    };

    match args.cmd {
        ObserveCmd::Logs {
            service,
            follow,
            tail,
            timestamp,
        } => {
            // Validate that the service exists (optional: could warn instead)
            if !args.dry_run {
                validate_service_exists(&root, &service)?;
            }

            let values = LogsValues {
                service,
                follow,
                tail,
                timestamp,
            };

            backend.logs(&values)?;
        }

        ObserveCmd::PortForward {
            service,
            local,
            remote,
        } => {
            // Resolve remote port from tonin.toml if not specified
            let remote_port = if let Some(rport) = remote {
                rport
            } else {
                resolve_service_port(&root, &service)?
            };

            // Default local port to remote port if not specified
            let local_port = local.unwrap_or(remote_port);

            let values = PortForwardValues {
                service,
                local_port,
                remote_port,
            };

            backend.port_forward(&values)?;
        }
    }

    Ok(())
}

/// Check if a service exists in the workspace.
fn validate_service_exists(root: &std::path::Path, service_name: &str) -> Result<()> {
    let plans = Plan::load_workspace(root)?;

    if !plans.iter().any(|p| p.name == service_name) {
        anyhow::bail!(
            "service '{}' not found in workspace {}",
            service_name,
            root.display()
        );
    }

    Ok(())
}

/// Resolve the primary port for a service from its tonin.toml.
fn resolve_service_port(root: &std::path::Path, service_name: &str) -> Result<u16> {
    let plans = Plan::load_workspace(root)?;

    let plan = plans
        .iter()
        .find(|p| p.name == service_name)
        .ok_or_else(|| anyhow::anyhow!("service '{}' not found in workspace", service_name))?;

    // Return the primary port
    Ok(plan.port as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observe_logs_args() {
        let args = ObserveArgs {
            workspace: None,
            dry_run: true,
            cmd: ObserveCmd::Logs {
                service: "users-service".to_string(),
                follow: true,
                tail: 10,
                timestamp: false,
            },
        };

        match args.cmd {
            ObserveCmd::Logs {
                service,
                follow,
                tail,
                ..
            } => {
                assert_eq!(service, "users-service");
                assert!(follow);
                assert_eq!(tail, 10);
            }
            _ => panic!("expected Logs variant"),
        }
    }

    #[test]
    fn test_observe_port_forward_args() {
        let args = ObserveArgs {
            workspace: None,
            dry_run: true,
            cmd: ObserveCmd::PortForward {
                service: "orders-service".to_string(),
                local: Some(8001),
                remote: Some(50051),
            },
        };

        match args.cmd {
            ObserveCmd::PortForward {
                service,
                local,
                remote,
            } => {
                assert_eq!(service, "orders-service");
                assert_eq!(local, Some(8001));
                assert_eq!(remote, Some(50051));
            }
            _ => panic!("expected PortForward variant"),
        }
    }

    #[test]
    fn test_observe_port_forward_defaults() {
        let args = ObserveArgs {
            workspace: None,
            dry_run: true,
            cmd: ObserveCmd::PortForward {
                service: "products-service".to_string(),
                local: None,
                remote: Some(8080),
            },
        };

        match args.cmd {
            ObserveCmd::PortForward { local, remote, .. } => {
                assert_eq!(local, None);
                assert_eq!(remote, Some(8080));
            }
            _ => panic!("expected PortForward variant"),
        }
    }

    #[test]
    fn test_dry_run_flag() {
        let args = ObserveArgs {
            workspace: None,
            dry_run: true,
            cmd: ObserveCmd::Logs {
                service: "test-service".to_string(),
                follow: false,
                tail: 5,
                timestamp: true,
            },
        };

        assert!(args.dry_run);
    }
}
