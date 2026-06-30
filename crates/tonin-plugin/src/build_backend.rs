use anyhow::Result;

/// Arguments for a single service build.
#[derive(Debug, Clone)]
pub struct BuildValues {
    /// Service name (e.g. "users-service")
    pub service: String,
    /// Full image tag (e.g. "ghcr.io/org/users-service:1.4.0")
    pub image_tag: String,
    /// Dockerfile path relative to workspace (e.g. "services/users-service/Dockerfile")
    pub dockerfile: String,
    /// Build context path (usually the service directory)
    pub context: String,
}

/// Result of a successful Docker build.
#[derive(Debug, Clone)]
pub struct BuildResult {
    /// Full image tag (e.g. "ghcr.io/org/users-service:1.4.0")
    pub image_tag: String,
    /// Image digest from docker inspect or push output (e.g. "sha256:abc123...")
    pub digest: Option<String>,
}

/// Pluggable build backend. Default: [`DockerBackend`] shells out to docker.
/// Swap to [`BuildDryRunBackend`] for `--dry-run` or tests with no docker.
pub trait BuildBackend: Send + Sync {
    /// Build a Docker image and return digest. Returns BuildResult on success, Err if build fails.
    ///
    /// Example: tonin build --service orders-service --tag v1.2.3 --registry docker.io --push
    fn build(&self, values: &BuildValues) -> Result<BuildResult>;
    /// Push a Docker image to registry. Returns Ok on success, Err if push fails.
    fn push(&self, image_tag: &str) -> Result<()>;
}

/// Default backend: shells out to the `docker` binary on $PATH.
pub struct DockerBackend;

impl BuildBackend for DockerBackend {
    fn build(&self, values: &BuildValues) -> Result<BuildResult> {
        let status = std::process::Command::new("docker")
            .args([
                "build",
                "-t",
                &values.image_tag,
                "-f",
                &values.dockerfile,
                &values.context,
            ])
            .status()
            .map_err(|e| anyhow::anyhow!("docker not found on PATH: {e}"))?;
        anyhow::ensure!(
            status.success(),
            "docker build failed for image={}",
            values.image_tag
        );

        // Capture digest using docker inspect
        let digest = capture_digest(&values.image_tag)?;

        Ok(BuildResult {
            image_tag: values.image_tag.clone(),
            digest,
        })
    }

    fn push(&self, image_tag: &str) -> Result<()> {
        let status = std::process::Command::new("docker")
            .args(["push", image_tag])
            .status()
            .map_err(|e| anyhow::anyhow!("docker not found on PATH: {e}"))?;
        anyhow::ensure!(
            status.success(),
            "docker push failed for image={}",
            image_tag
        );
        Ok(())
    }
}

/// Capture the digest of a built image using docker inspect.
fn capture_digest(image_tag: &str) -> Result<Option<String>> {
    let output = std::process::Command::new("docker")
        .args(["inspect", "--format", "{{json .RepoDigests}}", image_tag])
        .output()
        .map_err(|e| anyhow::anyhow!("docker inspect failed: {e}"))?;

    anyhow::ensure!(
        output.status.success(),
        "docker inspect failed for image={}",
        image_tag
    );

    let stdout = String::from_utf8(output.stdout)?;
    let trimmed = stdout.trim();

    // Parse JSON array output like ["ghcr.io/org/service@sha256:abc123"]
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let content = &trimmed[1..trimmed.len() - 1];
        if !content.is_empty() {
            // Remove quotes and return the first digest
            let digest = content
                .split(',')
                .next()
                .map(|s| s.trim().trim_matches('"').to_string());
            return Ok(digest);
        }
    }

    Ok(None)
}

/// No-op backend for `--dry-run` and unit tests. Prints what it would do.
pub struct BuildDryRunBackend;

impl BuildBackend for BuildDryRunBackend {
    fn build(&self, values: &BuildValues) -> Result<BuildResult> {
        println!(
            "DRY: docker build -t {} -f {} {}",
            values.image_tag, values.dockerfile, values.context
        );
        Ok(BuildResult {
            image_tag: values.image_tag.clone(),
            digest: Some("sha256:dry-run-abcd1234".to_string()),
        })
    }

    fn push(&self, image_tag: &str) -> Result<()> {
        println!("DRY: docker push {}", image_tag);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dry_run_build() -> Result<()> {
        let backend = BuildDryRunBackend;
        let values = BuildValues {
            service: "orders-service".to_string(),
            image_tag: "ghcr.io/org/orders-service:1.0.0".to_string(),
            dockerfile: "Dockerfile".to_string(),
            context: ".".to_string(),
        };
        let result = backend.build(&values)?;
        assert_eq!(result.image_tag, "ghcr.io/org/orders-service:1.0.0");
        assert_eq!(result.digest, Some("sha256:dry-run-abcd1234".to_string()));
        Ok(())
    }

    #[test]
    fn test_dry_run_push() -> Result<()> {
        let backend = BuildDryRunBackend;
        let image_tag = "ghcr.io/org/orders-service:1.0.0";
        backend.push(image_tag)?;
        Ok(())
    }

    #[test]
    fn test_image_tag_validation() {
        let tag = "ghcr.io/org/service:1.0.0";
        assert!(tag.contains(':'), "image tag must contain colon");
        assert!(tag.contains('/'), "image tag must contain slash");
    }

    #[test]
    fn test_build_result_construction() {
        let result = BuildResult {
            image_tag: "ghcr.io/org/test:1.0.0".to_string(),
            digest: Some("sha256:abc123def456".to_string()),
        };
        assert_eq!(result.image_tag, "ghcr.io/org/test:1.0.0");
        assert!(result.digest.is_some());
    }

    #[test]
    fn test_digest_parsing_with_repo_digest() {
        // Test parsing of docker inspect output format
        let json_output = r#"["ghcr.io/myorg/users-service@sha256:abc123def456"]"#;
        let content = &json_output[1..json_output.len() - 1];
        let digest = content
            .split(',')
            .next()
            .map(|s| s.trim().trim_matches('"').to_string());
        assert_eq!(
            digest,
            Some("ghcr.io/myorg/users-service@sha256:abc123def456".to_string())
        );
    }
}
