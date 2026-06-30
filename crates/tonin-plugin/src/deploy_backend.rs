use std::time::Duration;

use anyhow::Result;

/// Arguments for a single service upgrade.
#[derive(Debug, Clone)]
pub struct DeployValues {
    /// Image reference with digest: "ghcr.io/org/svc@sha256:..."
    pub image: String,
    /// Version string for helm `--set image.tag=<VERSION>`
    pub version: String,
    /// Helm release name (e.g. "users-service-staging")
    pub release: String,
    /// Kubernetes namespace
    pub namespace: String,
    /// Per-service upgrade timeout
    pub timeout: Duration,
}

/// Pluggable deploy backend. Default: [`HelmBackend`] shells out to helm.
/// Swap to [`DryRunBackend`] for `--dry-run` or tests with no cluster.
pub trait DeployBackend: Send + Sync {
    /// Upgrade (or install) a service. Returns Ok on success, Err if gate fails.
    fn upgrade(&self, values: &DeployValues) -> Result<()>;
    /// Roll back a release to its previous revision.
    fn rollback(&self, release: &str, namespace: &str) -> Result<()>;
    /// Return the currently-running image tag/version, or None if not deployed.
    fn running_version(&self, release: &str, namespace: &str) -> Result<Option<String>>;
}

/// Default backend: shells out to the `helm` binary on $PATH.
pub struct HelmBackend;

impl DeployBackend for HelmBackend {
    fn upgrade(&self, v: &DeployValues) -> Result<()> {
        let timeout = format!("{}s", v.timeout.as_secs());
        let status = std::process::Command::new("helm")
            .args([
                "upgrade",
                "--install",
                &v.release,
                "chart",
                "--namespace",
                &v.namespace,
                "--set",
                &format!("image.digest={}", v.image),
                "--set",
                &format!("image.tag={}", v.version),
                "--wait",
                "--timeout",
                &timeout,
                "--atomic",
                "--cleanup-on-fail",
            ])
            .status()
            .map_err(|e| anyhow::anyhow!("helm not found on PATH: {e}"))?;
        anyhow::ensure!(
            status.success(),
            "helm upgrade failed for release={}",
            v.release
        );
        Ok(())
    }

    fn rollback(&self, release: &str, namespace: &str) -> Result<()> {
        let status = std::process::Command::new("helm")
            .args(["rollback", release, "--namespace", namespace])
            .status()
            .map_err(|e| anyhow::anyhow!("helm rollback failed: {e}"))?;
        anyhow::ensure!(status.success(), "helm rollback failed for {release}");
        Ok(())
    }

    fn running_version(&self, release: &str, namespace: &str) -> Result<Option<String>> {
        let out = std::process::Command::new("helm")
            .args(["list", "--namespace", namespace, "-o", "json"])
            .output()
            .map_err(|e| anyhow::anyhow!("helm list failed: {e}"))?;
        if !out.status.success() {
            return Ok(None);
        }
        let releases: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Array(vec![]));
        Ok(releases
            .as_array()
            .and_then(|arr| arr.iter().find(|r| r["name"].as_str() == Some(release)))
            .and_then(|r| r["app_version"].as_str())
            .map(str::to_string))
    }
}

/// No-op backend for `--dry-run` and unit tests. Prints what it would do.
pub struct DryRunBackend;

impl DeployBackend for DryRunBackend {
    fn upgrade(&self, v: &DeployValues) -> Result<()> {
        println!(
            "DRY: helm upgrade --install {} (image={} version={})",
            v.release, v.image, v.version
        );
        Ok(())
    }
    fn rollback(&self, release: &str, namespace: &str) -> Result<()> {
        println!("DRY: helm rollback {release} --namespace {namespace}");
        Ok(())
    }
    fn running_version(&self, _release: &str, _namespace: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Pluggable telemetry reporter for deploy events.
pub trait TelemetryReporter: Send + Sync {
    fn deploy_event(&self, event: &DeployEvent);
}

#[derive(Debug)]
pub struct DeployEvent {
    pub service: String,
    pub version: String,
    pub env: String,
    pub wave: usize,
    pub status: DeployStatus,
    pub git_sha: String,
    pub image: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployStatus {
    Success,
    Failed,
    RolledBack,
}

impl DeployStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
        }
    }
}

/// No-op reporter used until M3 wires the real OTLP reporter.
pub struct NoopReporter;
impl TelemetryReporter for NoopReporter {
    fn deploy_event(&self, _event: &DeployEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_backend_upgrade_does_not_shell_out() {
        let backend = DryRunBackend;
        let values = DeployValues {
            image: "ghcr.io/myorg/users-service@sha256:abc".into(),
            version: "1.0.0".into(),
            release: "users-service-staging".into(),
            namespace: "myapp-staging".into(),
            timeout: Duration::from_secs(300),
        };
        // Must not panic or error (no helm needed)
        assert!(backend.upgrade(&values).is_ok());
        assert!(
            backend
                .rollback("users-service-staging", "myapp-staging")
                .is_ok()
        );
        assert_eq!(
            backend
                .running_version("users-service-staging", "myapp-staging")
                .unwrap(),
            None
        );
    }

    #[test]
    fn deploy_status_as_str() {
        assert_eq!(DeployStatus::Success.as_str(), "success");
        assert_eq!(DeployStatus::RolledBack.as_str(), "rolled_back");
    }
}
