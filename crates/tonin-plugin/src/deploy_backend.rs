use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;

use crate::plan::Plan;

/// Arguments for a single service upgrade.
#[derive(Debug, Clone)]
pub struct DeployValues {
    /// Image from the environment lock, normally pinned by digest:
    /// "ghcr.io/org/svc@sha256:...".
    pub image: String,
    /// Service version from the lock. Set as `image.version` (the
    /// `app.kubernetes.io/version` label and OTEL `service.version`), and used
    /// as the image tag when `image` carries neither a digest nor a tag.
    pub version: String,
    /// Helm release name: the service name (`plan.name`), the same release
    /// `tonin helm upgrade` manages.
    pub release: String,
    /// Kubernetes namespace
    pub namespace: String,
    /// Chart directory (absolute: `<service dir>/chart`).
    pub chart: PathBuf,
    /// Values files, applied in order: `values.yaml`, then `values-<env>.yaml`.
    pub values_files: Vec<PathBuf>,
    /// Per-service upgrade timeout
    pub timeout: Duration,
}

impl DeployValues {
    /// Values for deploying `plan` (loaded for `env`) at the locked `image`.
    pub fn for_plan(
        plan: &Plan,
        env: &str,
        image: impl Into<String>,
        version: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        let chart = plan.dir.join("chart");
        let chart = std::path::absolute(&chart).unwrap_or(chart);
        Self {
            image: image.into(),
            version: version.into(),
            release: plan.name.clone(),
            namespace: plan.namespace.clone(),
            values_files: vec![
                chart.join("values.yaml"),
                chart.join(format!("values-{env}.yaml")),
            ],
            chart,
            timeout,
        }
    }

    /// Whether a release running `running_image` (as
    /// [`DeployBackend::running_image`] returns it) already runs this image.
    /// Images are compared, not versions: a chart's appVersion comes from the
    /// tonin.toml it was generated from and says nothing about the digest.
    pub fn is_running(&self, running_image: Option<&str>) -> bool {
        running_image
            .is_some_and(|r| ImageRef::parse(r).to_string() == self.image_ref().to_string())
    }

    /// The image the release should run, as the chart renders it.
    pub fn image_ref(&self) -> ImageRef {
        let mut image = ImageRef::parse(&self.image);
        if image.digest.is_none() && image.tag.is_none() {
            image.tag = Some(self.version.clone());
        }
        image
    }

    /// Arguments for `helm` (without the binary) that apply this upgrade.
    ///
    /// The image is set from the lock: `image.repository` plus `image.digest`
    /// for a digest-pinned image (no mutable tag is set), else `image.tag`;
    /// `image.version` is always the lock's version.
    pub fn helm_upgrade_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "upgrade".into(),
            "--install".into(),
            self.release.clone(),
            self.chart.to_string_lossy().into_owned(),
        ];
        for file in &self.values_files {
            args.push("-f".into());
            args.push(file.to_string_lossy().into_owned());
        }
        args.extend(["--namespace".into(), self.namespace.clone()]);

        let image = self.image_ref();
        args.extend([
            "--set-string".into(),
            format!("image.repository={}", image.repository),
        ]);
        match (&image.digest, &image.tag) {
            (Some(digest), _) => {
                args.extend(["--set-string".into(), format!("image.digest={digest}")]);
            }
            (None, Some(tag)) => {
                args.extend(["--set-string".into(), format!("image.tag={tag}")]);
            }
            (None, None) => {}
        }
        args.extend([
            "--set-string".into(),
            format!("image.version={}", self.version),
        ]);

        args.extend([
            "--wait".into(),
            "--timeout".into(),
            format!("{}s", self.timeout.as_secs()),
            "--atomic".into(),
            "--cleanup-on-fail".into(),
        ]);
        args
    }
}

/// A container image reference split into repository, tag and digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// e.g. "ghcr.io/org/svc" (may include a registry port).
    pub repository: String,
    /// e.g. "1.4.0"
    pub tag: Option<String>,
    /// e.g. "sha256:abc…"
    pub digest: Option<String>,
}

impl ImageRef {
    /// Parse `repo[:tag][@digest]`. A `:` before the last `/` is a registry
    /// port, not a tag.
    pub fn parse(image: &str) -> Self {
        let (name, digest) = match image.split_once('@') {
            Some((name, digest)) => (name, Some(digest.to_string())),
            None => (image, None),
        };
        let last_slash = name.rfind('/').map_or(0, |i| i + 1);
        let (repository, tag) = match name[last_slash..].rfind(':') {
            Some(i) => (
                &name[..last_slash + i],
                Some(name[last_slash + i + 1..].to_string()),
            ),
            None => (name, None),
        };
        Self {
            repository: repository.to_string(),
            tag,
            digest,
        }
    }
}

/// How the chart renders the image: `repository@digest` when pinned, else
/// `repository:tag`.
impl fmt::Display for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.digest, &self.tag) {
            (Some(digest), _) => write!(f, "{}@{digest}", self.repository),
            (None, Some(tag)) => write!(f, "{}:{tag}", self.repository),
            (None, None) => f.write_str(&self.repository),
        }
    }
}

/// Pluggable deploy backend. Default: [`HelmBackend`] shells out to helm.
/// Swap to [`DryRunBackend`] for `--dry-run` or tests with no cluster.
pub trait DeployBackend: Send + Sync {
    /// Upgrade (or install) a service. Returns Ok on success, Err if gate fails.
    fn upgrade(&self, values: &DeployValues) -> Result<()>;
    /// Roll back a release to its previous revision.
    fn rollback(&self, release: &str, namespace: &str) -> Result<()>;
    /// Return the release's chart `appVersion`, or None if not deployed.
    /// This is the version of the chart that was installed, not necessarily
    /// the image that runs; compare images with [`Self::running_image`].
    fn running_version(&self, release: &str, namespace: &str) -> Result<Option<String>>;
    /// Return the image the release runs (as [`ImageRef`] renders it, e.g.
    /// `repo@sha256:…`), or None if not deployed / unknown.
    fn running_image(&self, _release: &str, _namespace: &str) -> Result<Option<String>> {
        Ok(None)
    }
    /// Return the version the release's values report (`image.version`, set
    /// by `tonin deploy` from the lock, else `image.tag`: what the chart's
    /// `app.kubernetes.io/version` label shows), or None if not deployed.
    fn running_release_version(&self, _release: &str, _namespace: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Default backend: shells out to the `helm` binary on $PATH.
pub struct HelmBackend;

impl DeployBackend for HelmBackend {
    fn upgrade(&self, v: &DeployValues) -> Result<()> {
        let status = std::process::Command::new("helm")
            .args(v.helm_upgrade_args())
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

    fn running_image(&self, release: &str, namespace: &str) -> Result<Option<String>> {
        Ok(helm_release_values(release, namespace)?
            .and_then(|values| image_from_values(&values))
            .map(|i| i.to_string()))
    }

    fn running_release_version(&self, release: &str, namespace: &str) -> Result<Option<String>> {
        Ok(helm_release_values(release, namespace)?.and_then(|v| version_from_values(&v)))
    }
}

/// `helm get values --all` of a release, or None when it is not installed
/// (or not reachable).
fn helm_release_values(release: &str, namespace: &str) -> Result<Option<serde_json::Value>> {
    let out = std::process::Command::new("helm")
        .args(["get", "values", release, "--namespace", namespace])
        .args(["--all", "--output", "json"])
        .output()
        .map_err(|e| anyhow::anyhow!("helm get values failed: {e}"))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&out.stdout)?))
}

/// The version a release's values report (see the chart's `service.version`).
fn version_from_values(values: &serde_json::Value) -> Option<String> {
    ["version", "tag"]
        .iter()
        .find_map(|key| match &values["image"][key] {
            serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
}

/// The image a release's values render (see the chart's `service.image`).
fn image_from_values(values: &serde_json::Value) -> Option<ImageRef> {
    let image = &values["image"];
    let non_empty = |key: &str| {
        image[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(ImageRef {
        repository: non_empty("repository")?,
        digest: non_empty("digest"),
        tag: non_empty("tag"),
    })
}

/// No-op backend for `--dry-run` and unit tests. Prints what it would do.
pub struct DryRunBackend;

impl DeployBackend for DryRunBackend {
    fn upgrade(&self, v: &DeployValues) -> Result<()> {
        println!("DRY: helm {}", v.helm_upgrade_args().join(" "));
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

    fn values(image: &str) -> DeployValues {
        DeployValues {
            image: image.into(),
            version: "1.0.0".into(),
            release: "users-service".into(),
            namespace: "myapp-prd".into(),
            chart: PathBuf::from("/ws/users/chart"),
            values_files: vec![
                PathBuf::from("/ws/users/chart/values.yaml"),
                PathBuf::from("/ws/users/chart/values-prd.yaml"),
            ],
            timeout: Duration::from_secs(300),
        }
    }

    #[test]
    fn image_ref_parses_digest_tag_and_registry_port() {
        let r = ImageRef::parse("ghcr.io/org/svc@sha256:abc");
        assert_eq!(r.repository, "ghcr.io/org/svc");
        assert_eq!(r.digest.as_deref(), Some("sha256:abc"));
        assert_eq!(r.tag, None);
        assert_eq!(r.to_string(), "ghcr.io/org/svc@sha256:abc");

        let r = ImageRef::parse("localhost:5000/svc:1.2.3");
        assert_eq!(r.repository, "localhost:5000/svc");
        assert_eq!(r.tag.as_deref(), Some("1.2.3"));
        assert_eq!(r.to_string(), "localhost:5000/svc:1.2.3");

        let r = ImageRef::parse("ghcr.io/org/svc:1.2.3@sha256:abc");
        assert_eq!(r.tag.as_deref(), Some("1.2.3"));
        // The digest wins, exactly like the chart's service.image helper.
        assert_eq!(r.to_string(), "ghcr.io/org/svc@sha256:abc");

        let r = ImageRef::parse("localhost:5000/svc");
        assert_eq!(r.repository, "localhost:5000/svc");
        assert_eq!(r.tag, None);
    }

    #[test]
    fn upgrade_args_pin_digest_without_a_tag() {
        let args = values("ghcr.io/myorg/users-service@sha256:abc").helm_upgrade_args();
        assert_eq!(
            args,
            [
                "upgrade",
                "--install",
                "users-service",
                "/ws/users/chart",
                "-f",
                "/ws/users/chart/values.yaml",
                "-f",
                "/ws/users/chart/values-prd.yaml",
                "--namespace",
                "myapp-prd",
                "--set-string",
                "image.repository=ghcr.io/myorg/users-service",
                "--set-string",
                "image.digest=sha256:abc",
                "--set-string",
                "image.version=1.0.0",
                "--wait",
                "--timeout",
                "300s",
                "--atomic",
                "--cleanup-on-fail",
            ]
        );
        assert!(!args.iter().any(|a| a.starts_with("image.tag")));
    }

    #[test]
    fn upgrade_args_always_carry_the_lock_version() {
        for image in [
            "ghcr.io/myorg/users-service@sha256:abc",
            "ghcr.io/myorg/users-service:latest",
            "ghcr.io/myorg/users-service",
        ] {
            let args = values(image).helm_upgrade_args();
            assert!(
                args.contains(&"image.version=1.0.0".to_string()),
                "{args:?}"
            );
        }
    }

    #[test]
    fn upgrade_args_fall_back_to_a_tag_without_a_digest() {
        let args = values("ghcr.io/myorg/users-service:1.0.0").helm_upgrade_args();
        assert!(args.contains(&"image.tag=1.0.0".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a.starts_with("image.digest")));

        // Neither digest nor tag: the lock version is the tag.
        let args = values("ghcr.io/myorg/users-service").helm_upgrade_args();
        assert!(args.contains(&"image.tag=1.0.0".to_string()), "{args:?}");
    }

    #[test]
    fn image_from_release_values_matches_the_chart_helper() {
        let pinned = serde_json::json!({"image": {
            "repository": "ghcr.io/o/s", "tag": "0.9.0", "digest": "sha256:abc"}});
        assert_eq!(
            image_from_values(&pinned).unwrap().to_string(),
            "ghcr.io/o/s@sha256:abc"
        );
        let tagged = serde_json::json!({"image": {
            "repository": "ghcr.io/o/s", "tag": "0.9.0", "digest": ""}});
        assert_eq!(
            image_from_values(&tagged).unwrap().to_string(),
            "ghcr.io/o/s:0.9.0"
        );
        assert!(image_from_values(&serde_json::json!({})).is_none());
    }

    #[test]
    fn release_version_prefers_image_version_over_the_tag() {
        let pinned = serde_json::json!({"image": {
            "repository": "ghcr.io/o/s", "tag": "0.9.0", "version": "1.4.0",
            "digest": "sha256:abc"}});
        assert_eq!(version_from_values(&pinned).as_deref(), Some("1.4.0"));
        let tagged = serde_json::json!({"image": {"tag": "0.9.0", "version": ""}});
        assert_eq!(version_from_values(&tagged).as_deref(), Some("0.9.0"));
        assert_eq!(version_from_values(&serde_json::json!({})), None);
    }

    #[test]
    fn is_running_compares_images_not_versions() {
        let v = values("ghcr.io/myorg/users-service@sha256:abc");
        assert!(v.is_running(Some("ghcr.io/myorg/users-service@sha256:abc")));
        // A tag next to the digest does not matter: the digest is what runs.
        assert!(v.is_running(Some("ghcr.io/myorg/users-service:0.9.0@sha256:abc")));
        assert!(!v.is_running(Some("ghcr.io/myorg/users-service@sha256:fff")));
        assert!(!v.is_running(Some("ghcr.io/myorg/users-service:1.0.0")));
        assert!(!v.is_running(None));
    }

    #[test]
    fn dry_run_backend_upgrade_does_not_shell_out() {
        let backend = DryRunBackend;
        let values = values("ghcr.io/myorg/users-service@sha256:abc");
        // Must not panic or error (no helm needed)
        assert!(backend.upgrade(&values).is_ok());
        assert!(backend.rollback("users-service", "myapp-prd").is_ok());
        assert_eq!(
            backend
                .running_version("users-service", "myapp-prd")
                .unwrap(),
            None
        );
        assert_eq!(
            backend.running_image("users-service", "myapp-prd").unwrap(),
            None
        );
    }

    #[test]
    fn deploy_status_as_str() {
        assert_eq!(DeployStatus::Success.as_str(), "success");
        assert_eq!(DeployStatus::RolledBack.as_str(), "rolled_back");
    }
}
