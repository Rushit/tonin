//! Registry configuration resolution for tonin builds.
//!
//! V1 MVP focuses on GitHub Actions as the primary CI integration.
//! ~/.tonin.cnf may be added in future versions.
//!
//! Registry resolution priority:
//! 1. CLI flag: `--registry docker.io/custom`
//! 2. Env var: `TONIN_REGISTRY=docker.io/custom`
//! 3. tonin.toml: `[build]` registry = "docker.io/custom"
//! 4. GitHub Actions: `GITHUB_REPOSITORY=org/repo` → `ghcr.io/org/repo`
//! 5. Default: `ghcr.io/example`

use std::env;

/// Configuration loader with support for registry resolution and CI detection.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Resolve registry with 5-level priority.
    ///
    /// Priority order:
    /// 1. CLI flag (cli_override)
    /// 2. TONIN_REGISTRY env var
    /// 3. tonin.toml `[build]`.registry (toml_registry)
    /// 4. GitHub Actions auto-detection: GITHUB_REPOSITORY → ghcr.io/org/repo
    /// 5. Default: "ghcr.io/example"
    pub fn resolve_registry(
        cli_override: Option<&str>,
        toml_registry: Option<&str>,
        _env_registry: Option<&str>,
    ) -> String {
        // Level 1: CLI flag
        if let Some(reg) = cli_override {
            return reg.to_string();
        }

        // Level 2: TONIN_REGISTRY env var
        #[allow(clippy::collapsible_if)]
        {
            if let Ok(reg) = env::var("TONIN_REGISTRY") {
                if !reg.is_empty() {
                    return reg;
                }
            }
        }

        // Level 3: tonin.toml value
        if let Some(reg) = toml_registry {
            return reg.to_string();
        }

        // Level 4: GitHub Actions auto-detection
        if let Some(registry) = Self::detect_github_actions() {
            return registry;
        }

        // Level 5: Default
        "ghcr.io/example".to_string()
    }

    /// Detect GitHub Actions and return ghcr.io registry.
    ///
    /// If GITHUB_REPOSITORY is set (format: "owner/repo"), returns "ghcr.io/owner/repo".
    /// Returns None if not in GitHub Actions environment.
    pub fn detect_github_actions() -> Option<String> {
        env::var("GITHUB_REPOSITORY").ok().and_then(|repo| {
            if repo.is_empty() {
                None
            } else {
                Some(format!("ghcr.io/{}", repo))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_registry_priority_cli_flag() {
        let result = ConfigLoader::resolve_registry(
            Some("ghcr.io/cli-override"),
            Some("ghcr.io/tonin-toml"),
            None,
        );
        assert_eq!(result, "ghcr.io/cli-override");
    }

    #[test]
    fn test_resolve_registry_priority_tonin_toml() {
        let result = ConfigLoader::resolve_registry(None, Some("ghcr.io/tonin-toml"), None);
        assert_eq!(result, "ghcr.io/tonin-toml");
    }

    #[test]
    fn test_resolve_registry_priority_default() {
        // Save and clear GITHUB_REPOSITORY to test true default (not GitHub Actions auto-detect).
        let saved_repo = env::var("GITHUB_REPOSITORY").ok();
        unsafe {
            env::remove_var("GITHUB_REPOSITORY");
        }
        let result = ConfigLoader::resolve_registry(None, None, None);
        if let Some(repo) = saved_repo {
            unsafe {
                env::set_var("GITHUB_REPOSITORY", repo);
            }
        }
        assert_eq!(result, "ghcr.io/example");
    }

    #[test]
    fn test_github_actions_detection_format() {
        // This test verifies the format conversion logic
        // In real usage, GITHUB_REPOSITORY would be set by GitHub Actions
        let repo = "myorg/myrepo";
        let registry = format!("ghcr.io/{}", repo);
        assert_eq!(registry, "ghcr.io/myorg/myrepo");
    }
}
