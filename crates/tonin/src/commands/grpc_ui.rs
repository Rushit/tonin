//! `tonin grpc-ui` — Launch grpcui for interactive gRPC service inspection.
//!
//! Opens a web-based UI for exploring and testing gRPC services running locally.
//! Assumes the target service is reachable at localhost:PORT via plaintext gRPC.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use tonin_plugin::{GrpcUiBackend, Plan};

/// Launch grpcui for interactive gRPC service inspection.
///
/// Opens a web-based UI for exploring and testing gRPC services running locally.
/// Useful for debugging and testing gRPC APIs without writing client code.
///
///   tonin grpc-ui --service users-service          # auto-detect port from tonin.toml
///   tonin grpc-ui --service users-service --port 50051  # explicit port
#[derive(Args)]
pub struct GrpcUiArgs {
    /// Service name (e.g. "users-service")
    #[arg(long)]
    pub service: String,

    /// gRPC service port (inferred from tonin.toml if not provided)
    #[arg(long)]
    pub port: Option<u16>,

    /// Workspace root containing tonin.toml files.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub fn run(args: GrpcUiArgs) -> Result<()> {
    let root = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    // Resolve the service port from tonin.toml if not explicitly provided
    let service_port = if let Some(port) = args.port {
        port
    } else {
        resolve_service_port(&root, &args.service)?
    };

    // Launch grpcui
    GrpcUiBackend::launch_grpcui(&args.service, service_port)
        .map_err(|e| anyhow::anyhow!("failed to launch grpcui: {}", e))
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
    fn test_grpc_ui_args_creation() {
        let args = GrpcUiArgs {
            service: "users-service".to_string(),
            port: Some(50051),
            workspace: None,
        };

        assert_eq!(args.service, "users-service");
        assert_eq!(args.port, Some(50051));
    }

    #[test]
    fn test_grpc_ui_args_without_port() {
        let args = GrpcUiArgs {
            service: "orders-service".to_string(),
            port: None,
            workspace: None,
        };

        assert_eq!(args.service, "orders-service");
        assert_eq!(args.port, None);
    }

    #[test]
    fn test_grpc_ui_args_with_workspace() {
        let args = GrpcUiArgs {
            service: "products-service".to_string(),
            port: Some(8080),
            workspace: Some(PathBuf::from("/path/to/workspace")),
        };

        assert_eq!(args.service, "products-service");
        assert_eq!(args.port, Some(8080));
        assert_eq!(args.workspace, Some(PathBuf::from("/path/to/workspace")));
    }

    #[test]
    fn test_grpc_ui_service_name_variations() {
        let service_names = vec![
            "auth-service",
            "users-api",
            "payment_processor",
            "notification-v2",
        ];

        for service in service_names {
            let args = GrpcUiArgs {
                service: service.to_string(),
                port: Some(50051),
                workspace: None,
            };

            assert_eq!(args.service, service);
        }
    }
}
