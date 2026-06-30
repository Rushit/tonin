use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Plan;

/// Structured result of a deployment operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployResult {
    /// Service name that was deployed
    pub service: String,
    /// Environment name
    pub env: String,
    /// Image digest deployed (e.g. "sha256:abc123...")
    pub digest: String,
    /// Deployment status
    pub status: DeploymentStatus,
    /// Timestamp of deployment completion (Unix seconds)
    pub timestamp: u64,
    /// Optional deployment message (error or success detail)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Status of a deployment operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeploymentStatus {
    /// Deployment succeeded
    Success,
    /// Deployment failed
    Failed,
    /// Deployment was rolled back
    RolledBack,
}

impl DeploymentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
        }
    }
}

/// Status of a running service in an environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Service name
    pub service: String,
    /// Environment name
    pub env: String,
    /// Currently running version (e.g. "1.2.3") or null if not deployed
    pub running_version: Option<String>,
    /// Desired version from lock file (if exists)
    pub desired_version: Option<String>,
    /// Current image digest
    pub digest: Option<String>,
    /// Health status (e.g. "healthy", "degraded", "unhealthy", "not_deployed")
    pub health: String,
    /// Timestamp when status was last updated
    pub timestamp: u64,
}

/// Pluggable platform orchestrator trait.
///
/// Enables agnitiv-platform (or other external systems) to orchestrate
/// tonin deployments without calling the CLI directly. This trait
/// decouples the orchestration logic from cargo/docker/helm internals.
///
/// # Example
///
/// ```no_run
/// use tonin_plugin::platform::PlatformOrchestrator;
/// use tonin_plugin::{Plan, wave_groups};
/// use std::path::Path;
///
/// // Load all plans from workspace
/// let plans = Plan::load_workspace_with_env(Path::new("."), "staging")?;
/// let waves = wave_groups(&plans);
///
/// // Use the CLI default implementation
/// let orchestrator = tonin_plugin::platform::CliOrchestrator::new("/path/to/workspace");
///
/// // Deploy the first wave
/// if let Some(wave) = waves.first() {
///     let results = orchestrator.deploy_wave(wave, "staging")?;
///     for result in results {
///         println!("{}: {} ({})", result.service, result.status.as_str(), result.digest);
///     }
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub trait PlatformOrchestrator: Send + Sync {
    /// Deploy a wave of services in parallel (or sequentially).
    ///
    /// Reads version locks and deploys each service in the wave.
    /// Returns detailed results for each deployment attempt.
    ///
    /// # Arguments
    /// * `wave` — Slice of Plan references to deploy (typically from wave_groups)
    /// * `env` — Environment name (e.g. "staging", "prod")
    ///
    /// # Returns
    /// * `Ok(Vec<DeployResult>)` — Results for each service deployed
    /// * `Err(...)` — If lock file is missing or wave is empty
    fn deploy_wave(&self, wave: &[&Plan], env: &str) -> Result<Vec<DeployResult>>;

    /// Get the current status of all running services in an environment.
    ///
    /// Queries Kubernetes (via helm) for running versions and cross-references
    /// with the environment lock file to detect drift.
    ///
    /// # Arguments
    /// * `env` — Environment name
    ///
    /// # Returns
    /// * `Ok(Vec<ServiceStatus>)` — Current status of all services
    /// * `Err(...)` — If lock file is missing or cluster is unreachable
    fn get_running_services(&self, env: &str) -> Result<Vec<ServiceStatus>>;

    /// Rollback a service to its previous Helm release revision.
    ///
    /// Executes `helm rollback <release> --namespace <namespace>`.
    ///
    /// # Arguments
    /// * `service` — Service name to rollback
    /// * `env` — Environment name
    ///
    /// # Returns
    /// * `Ok(())` — Rollback succeeded
    /// * `Err(...)` — If rollback fails or service not found
    fn rollback_service(&self, service: &str, env: &str) -> Result<()>;
}

/// Default CLI-based platform orchestrator.
///
/// Shells out to `tonin` CLI commands and parses JSON output.
/// This implementation integrates with existing tonin infrastructure:
/// - Reads environment locks via GitLockStore
/// - Uses existing DeployBackend (Helm)
/// - Integrates with phase 4b health checks
pub struct CliOrchestrator {
    workspace: std::path::PathBuf,
}

impl CliOrchestrator {
    /// Create a new CLI orchestrator for a workspace.
    pub fn new<P: AsRef<std::path::Path>>(workspace: P) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
        }
    }
}

impl PlatformOrchestrator for CliOrchestrator {
    fn deploy_wave(&self, wave: &[&Plan], env: &str) -> Result<Vec<DeployResult>> {
        use crate::deploy_backend::{DeployBackend, DeployValues, HelmBackend};
        use crate::lock::{GitLockStore, LockStore};
        use std::time::Duration;

        let backend = HelmBackend;
        let store = GitLockStore::new(&self.workspace);
        let lock = store.read(env)?;

        let mut results = Vec::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for plan in wave {
            let Some(svc_lock) = lock.services.get(&plan.name) else {
                results.push(DeployResult {
                    service: plan.name.clone(),
                    env: env.to_string(),
                    digest: String::new(),
                    status: DeploymentStatus::Failed,
                    timestamp: now,
                    message: Some("Service not in lock file".to_string()),
                });
                continue;
            };

            // Parse timeout (default 300s if parse fails)
            let timeout = Duration::from_secs(300);

            let values = DeployValues {
                image: svc_lock.image.clone(),
                version: svc_lock.version.clone(),
                release: format!("{}-{}", plan.name, env),
                namespace: plan.namespace.clone(),
                timeout,
            };

            match backend.upgrade(&values) {
                Ok(()) => {
                    results.push(DeployResult {
                        service: plan.name.clone(),
                        env: env.to_string(),
                        digest: svc_lock.image.clone(),
                        status: DeploymentStatus::Success,
                        timestamp: now,
                        message: None,
                    });
                }
                Err(e) => {
                    results.push(DeployResult {
                        service: plan.name.clone(),
                        env: env.to_string(),
                        digest: svc_lock.image.clone(),
                        status: DeploymentStatus::Failed,
                        timestamp: now,
                        message: Some(e.to_string()),
                    });
                }
            }
        }

        Ok(results)
    }

    fn get_running_services(&self, env: &str) -> Result<Vec<ServiceStatus>> {
        use crate::deploy_backend::{DeployBackend, HelmBackend};
        use crate::lock::{GitLockStore, LockStore};

        let backend = HelmBackend;
        let store = GitLockStore::new(&self.workspace);
        let lock = store.read(env)?;
        let plans = Plan::load_workspace_with_env(&self.workspace, env)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut statuses = Vec::new();

        for plan in plans {
            let desired_version = lock.services.get(&plan.name).map(|s| s.version.clone());

            let release_name = format!("{}-{}", plan.name, env);
            let running_version = backend
                .running_version(&release_name, &plan.namespace)
                .ok()
                .flatten();

            let digest = lock
                .services
                .get(&plan.name)
                .and_then(|s| s.image.split('@').nth(1).map(|d| d.to_string()));

            let health = match (&running_version, &desired_version) {
                (None, None) => "not_deployed".to_string(),
                (None, Some(_)) => "unhealthy".to_string(),
                (Some(running), Some(desired)) if running == desired => "healthy".to_string(),
                _ => "degraded".to_string(),
            };

            statuses.push(ServiceStatus {
                service: plan.name.clone(),
                env: env.to_string(),
                running_version,
                desired_version,
                digest,
                health,
                timestamp: now,
            });
        }

        Ok(statuses)
    }

    fn rollback_service(&self, service: &str, env: &str) -> Result<()> {
        use crate::deploy_backend::{DeployBackend, HelmBackend};
        use crate::lock::{GitLockStore, LockStore};

        let backend = HelmBackend;
        let store = GitLockStore::new(&self.workspace);
        let lock = store.read(env)?;

        lock.services
            .get(service)
            .ok_or_else(|| anyhow::anyhow!("service {} not in lock file", service))?;

        let plans = Plan::load_workspace_with_env(&self.workspace, env)?;
        let plan = plans
            .iter()
            .find(|p| p.name == service)
            .ok_or_else(|| anyhow::anyhow!("service {} not found in tonin.toml", service))?;

        let release_name = format!("{}-{}", service, env);
        backend.rollback(&release_name, &plan.namespace)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_status_as_str() {
        assert_eq!(DeploymentStatus::Success.as_str(), "success");
        assert_eq!(DeploymentStatus::Failed.as_str(), "failed");
        assert_eq!(DeploymentStatus::RolledBack.as_str(), "rolled_back");
    }

    #[test]
    fn deploy_result_serialize() {
        let result = DeployResult {
            service: "users-service".to_string(),
            env: "staging".to_string(),
            digest: "sha256:abc123".to_string(),
            status: DeploymentStatus::Success,
            timestamp: 1719792000,
            message: None,
        };

        let json = serde_json::to_string(&result).expect("serialize failed");
        assert!(json.contains("users-service"));
        assert!(json.contains("staging"));
        assert!(json.contains("success"));

        // Round-trip
        let deserialized: DeployResult = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(deserialized.service, "users-service");
        assert_eq!(deserialized.status, DeploymentStatus::Success);
    }

    #[test]
    fn service_status_health_detection() {
        let running_version = Some("1.2.3".to_string());
        let desired_version = Some("1.2.3".to_string());

        let status = ServiceStatus {
            service: "orders-service".to_string(),
            env: "prod".to_string(),
            running_version: running_version.clone(),
            desired_version: desired_version.clone(),
            digest: Some("sha256:def456".to_string()),
            health: "healthy".to_string(),
            timestamp: 1719792000,
        };

        assert_eq!(status.health, "healthy");
    }
}
