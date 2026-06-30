use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use tonin_plugin::{
    BuildBackend, BuildDryRunBackend, BuildValues, ConfigLoader, DockerBackend, Plan,
};

#[derive(Args)]
pub struct BuildArgs {
    /// Service to build (if omitted, builds all services in workspace).
    #[arg(long)]
    pub service: Option<String>,

    /// Explicit image tag (e.g. "ghcr.io/org/svc:1.0.0").
    /// If omitted, resolves from VERSION file or git describe.
    #[arg(long)]
    pub tag: Option<String>,

    /// Registry override (e.g. "ghcr.io/myorg").
    /// Overrides registry from tonin.toml if present.
    #[arg(long)]
    pub registry: Option<String>,

    /// Also push image to registry after building.
    #[arg(long)]
    pub push: bool,

    /// Show what would build without running docker.
    #[arg(long)]
    pub dry_run: bool,

    /// Workspace root containing tonin.toml files.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

pub fn run(args: BuildArgs) -> Result<()> {
    let root = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let backend: Box<dyn BuildBackend> = if args.dry_run {
        Box::new(BuildDryRunBackend)
    } else {
        Box::new(DockerBackend)
    };

    // Resolve version from VERSION file or git
    let version = resolve_version(&root)?;

    // Load all plans from the workspace
    let plans = Plan::load_workspace(&root)?;

    // Filter plans to build (either specific service or all)
    let plans_to_build: Vec<&Plan> = match &args.service {
        Some(svc_name) => plans.iter().filter(|p| &p.name == svc_name).collect(),
        None => plans.iter().collect(),
    };

    anyhow::ensure!(!plans_to_build.is_empty(), "no services found to build");

    // Build each service
    let mut build_results = Vec::new();

    for plan in &plans_to_build {
        let image_tag = args.tag.clone().unwrap_or_else(|| {
            // Registry resolution with 5-level priority (V1 MVP):
            // 1. CLI flag (--registry)
            // 2. TONIN_REGISTRY env var
            // 3. tonin.toml [image].registry
            // 4. GitHub Actions: GITHUB_REPOSITORY → ghcr.io/org/repo
            // 5. Default: "ghcr.io/example"
            let registry = ConfigLoader::resolve_registry(
                args.registry.as_deref(),
                plan.image_registry.as_deref(),
                None,
            );
            format!("{}/{}:{}", registry, plan.name, version)
        });

        let dockerfile = infer_dockerfile(&root, &plan.name)?;
        let context = infer_context(&root, &plan.name);

        let values = BuildValues {
            service: plan.name.clone(),
            image_tag: image_tag.clone(),
            dockerfile: dockerfile.clone(),
            context: context.clone(),
        };

        println!("▶ building {}", plan.name);
        let build_result = backend.build(&values)?;
        build_results.push(build_result);

        // Single consolidated push block: handles both dry-run and regular builds
        if args.push {
            println!("  pushing {}", image_tag);
            backend.push(&image_tag)?;
        }
    }

    println!("✓ {} service(s) built successfully", plans_to_build.len());
    Ok(())
}

/// Resolve version from VERSION file, or fallback to git describe.
fn resolve_version(workspace: &Path) -> Result<String> {
    let version_path = workspace.join("VERSION");
    if version_path.exists() {
        let content = std::fs::read_to_string(&version_path)?.trim().to_string();
        if !content.is_empty() {
            return Ok(content);
        }
    }

    // Fallback to git describe --tags --always
    let output = std::process::Command::new("git")
        .args(["describe", "--tags", "--always"])
        .current_dir(workspace)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git describe: {e}"))?;

    anyhow::ensure!(output.status.success(), "git describe failed");

    let version = String::from_utf8(output.stdout)?.trim().to_string();

    anyhow::ensure!(
        !version.is_empty(),
        "could not resolve version from VERSION file or git"
    );

    Ok(version)
}

/// Infer Dockerfile path for a service.
/// Try standard locations: services/<SERVICE>/Dockerfile, <SERVICE>/Dockerfile, etc.
/// Returns error if no Dockerfile found in standard locations.
fn infer_dockerfile(workspace: &Path, service: &str) -> Result<String> {
    let candidates = vec![
        format!("services/{}/Dockerfile", service),
        format!("{}/Dockerfile", service),
    ];

    for candidate in &candidates {
        let path = workspace.join(candidate);
        if path.exists() {
            return Ok(candidate.clone());
        }
    }

    anyhow::bail!(
        "no Dockerfile found for service '{}' in standard locations (tried: {})",
        service,
        candidates.join(", ")
    )
}

/// Infer build context for a service.
/// Usually the service directory itself.
fn infer_context(workspace: &Path, service: &str) -> String {
    let candidates = vec![format!("services/{}", service), service.to_string()];

    for candidate in candidates {
        let path = workspace.join(&candidate);
        if path.exists() && path.is_dir() {
            return candidate;
        }
    }

    // Default fallback
    service.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_resolution_missing_workspace() {
        let result = resolve_version(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());
    }

    #[test]
    fn test_dockerfile_inference_missing_file() {
        // Test that Dockerfile validation fails for nonexistent service
        let result = infer_dockerfile(Path::new("/tmp"), "nonexistent-svc-xyz");
        assert!(
            result.is_err(),
            "should fail when no Dockerfile found for service"
        );
    }

    #[test]
    fn test_context_inference() {
        let context = infer_context(Path::new("/tmp"), "test-svc");
        assert!(
            context.contains("test-svc"),
            "Context should contain service name"
        );
    }

    #[test]
    fn test_build_values_construction() {
        let values = BuildValues {
            service: "orders-service".to_string(),
            image_tag: "ghcr.io/myorg/orders-service:1.2.3".to_string(),
            dockerfile: "services/orders-service/Dockerfile".to_string(),
            context: "services/orders-service".to_string(),
        };
        assert_eq!(values.service, "orders-service");
        assert_eq!(values.image_tag, "ghcr.io/myorg/orders-service:1.2.3");
        assert!(values.dockerfile.contains("Dockerfile"));
    }
}
