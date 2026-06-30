//! Platform client for agnitiv-platform to trigger tonin deployments.
//!
//! This module provides a client interface for orchestrating tonin deployments
//! without directly calling the tonin CLI. Can be extended to support gRPC
//! in future phases.

use serde::{Deserialize, Serialize};
use std::process::Command;

/// Configuration for the platform client.
#[derive(Debug, Clone)]
pub struct PlatformClientConfig {
    /// Path to tonin binary or "tonin" if on $PATH
    pub tonin_bin: String,
    /// Workspace root directory
    pub workspace: String,
}

impl Default for PlatformClientConfig {
    fn default() -> Self {
        Self {
            tonin_bin: "tonin".to_string(),
            workspace: ".".to_string(),
        }
    }
}

/// CLI-based platform client.
///
/// Calls tonin subcommands and parses JSON responses. Used by agnitiv-platform
/// to orchestrate deployments without direct dependency on tonin internals.
pub struct PlatformClient {
    config: PlatformClientConfig,
}

impl PlatformClient {
    /// Create a new platform client with default config.
    pub fn new() -> Self {
        Self {
            config: PlatformClientConfig::default(),
        }
    }

    /// Create a new platform client with custom config.
    pub fn with_config(config: PlatformClientConfig) -> Self {
        Self { config }
    }

    /// Deploy a service to an environment.
    ///
    /// Executes `tonin platform deploy --env <ENV> --service <SERVICE> --json`
    /// and parses the JSON response.
    ///
    /// # Arguments
    /// * `env` — Environment name (e.g. "staging", "prod")
    /// * `service` — Service name to deploy
    ///
    /// # Returns
    /// * `Ok(DeployResponse)` — Deployment result
    /// * `Err(...)` — If command fails or output is not valid JSON
    pub fn deploy_service(
        &self,
        env: &str,
        service: &str,
    ) -> Result<DeployResponse, Box<dyn std::error::Error>> {
        let output = Command::new(&self.config.tonin_bin)
            .args([
                "platform",
                "deploy",
                "--env",
                env,
                "--service",
                service,
                "--json",
                "--workspace",
                &self.config.workspace,
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tonin platform deploy failed: {}", stderr).into());
        }

        let response: DeployResponse = serde_json::from_slice(&output.stdout)?;
        Ok(response)
    }

    /// Get the status of all services in an environment.
    ///
    /// Executes `tonin platform status --env <ENV> --json` and parses the response.
    ///
    /// # Arguments
    /// * `env` — Environment name
    ///
    /// # Returns
    /// * `Ok(Vec<ServiceStatusResponse>)` — Status of all services
    /// * `Err(...)` — If command fails or output is invalid
    pub fn get_status(
        &self,
        env: &str,
    ) -> Result<Vec<ServiceStatusResponse>, Box<dyn std::error::Error>> {
        let output = Command::new(&self.config.tonin_bin)
            .args([
                "platform",
                "status",
                "--env",
                env,
                "--json",
                "--workspace",
                &self.config.workspace,
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tonin platform status failed: {}", stderr).into());
        }

        let response: Vec<ServiceStatusResponse> = serde_json::from_slice(&output.stdout)?;
        Ok(response)
    }

    /// Rollback a service to its previous release.
    ///
    /// Executes `tonin platform rollback --service <SERVICE> --env <ENV>`.
    ///
    /// # Arguments
    /// * `service` — Service name
    /// * `env` — Environment name
    ///
    /// # Returns
    /// * `Ok(())` — Rollback succeeded
    /// * `Err(...)` — If command fails
    pub fn rollback_service(
        &self,
        service: &str,
        env: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = Command::new(&self.config.tonin_bin)
            .args([
                "platform",
                "rollback",
                "--service",
                service,
                "--env",
                env,
                "--workspace",
                &self.config.workspace,
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tonin platform rollback failed: {}", stderr).into());
        }

        Ok(())
    }
}

impl Default for PlatformClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Response from a deployment command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployResponse {
    pub service: String,
    pub env: String,
    pub digest: String,
    pub status: String,
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Response containing service status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatusResponse {
    pub service: String,
    pub env: String,
    pub running_version: Option<String>,
    pub desired_version: Option<String>,
    pub digest: Option<String>,
    pub health: String,
    pub timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_client_config_default() {
        let config = PlatformClientConfig::default();
        assert_eq!(config.tonin_bin, "tonin");
        assert_eq!(config.workspace, ".");
    }

    #[test]
    fn platform_client_new() {
        let client = PlatformClient::new();
        assert_eq!(client.config.tonin_bin, "tonin");
    }

    #[test]
    fn deploy_response_serialize() {
        let response = DeployResponse {
            service: "users-service".to_string(),
            env: "staging".to_string(),
            digest: "sha256:abc123".to_string(),
            status: "success".to_string(),
            timestamp: 1719792000,
            message: None,
        };

        let json = serde_json::to_string(&response).expect("serialize failed");
        assert!(json.contains("users-service"));

        let deserialized: DeployResponse = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(deserialized.status, "success");
    }

    #[test]
    fn service_status_response_serialize() {
        let status = ServiceStatusResponse {
            service: "orders-service".to_string(),
            env: "prod".to_string(),
            running_version: Some("1.2.3".to_string()),
            desired_version: Some("1.2.3".to_string()),
            digest: Some("sha256:def456".to_string()),
            health: "healthy".to_string(),
            timestamp: 1719792000,
        };

        let json = serde_json::to_string(&status).expect("serialize failed");
        let deserialized: ServiceStatusResponse =
            serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(deserialized.health, "healthy");
    }
}
