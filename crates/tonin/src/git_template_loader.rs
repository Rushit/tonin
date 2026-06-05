//! Clone and cache tonin templates directly from git repositories.
//!
//! No need for GitHub releases or tarballs — just clone at a specific tag.
//! Cached locally at ~/.tonin/templates/{repo-name}/{version}/

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::process::Command;

const DEFAULT_REPO: &str = "https://github.com/Rushit/tonin-templates.git";

/// Get the cache directory for templates.
fn cache_dir(repo: &str, version: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    // Extract repo name from URL (e.g., "tonin-templates" or "tonin-templates-flat")
    let repo_name = repo
        .split('/')
        .last()
        .unwrap_or("tonin-templates")
        .trim_end_matches(".git");

    let cache_path = PathBuf::from(home)
        .join(".tonin")
        .join("templates")
        .join(repo_name)
        .join(version);

    Ok(cache_path)
}

/// Normalize git URL for consistency.
/// Accepts: https://github.com/Owner/repo.git, github.com/Owner/repo, Owner/repo
fn normalize_git_url(repo: &str) -> String {
    if repo.starts_with("https://") || repo.starts_with("git@") {
        repo.to_string()
    } else if repo.contains('/') {
        format!("https://github.com/{}.git", repo.trim_end_matches(".git"))
    } else {
        repo.to_string()
    }
}

/// Clone templates from a git repo and cache locally.
///
/// Repo: https://github.com/Owner/repo.git or Owner/repo
/// Version: git tag (e.g., "v0.4.0" or "latest")
pub fn load_templates(repo: Option<&str>, version: &str) -> Result<PathBuf> {
    let repo = repo.unwrap_or(DEFAULT_REPO);
    let repo_url = normalize_git_url(repo);
    let cache_path = cache_dir(&repo_url, version)?;

    // Already cached?
    if cache_path.exists() {
        eprintln!(
            "✓ Using cached templates from {}",
            cache_path.display()
        );
        return Ok(cache_path);
    }

    eprintln!("⬇ Cloning templates from {} @ {} ...", repo_url, version);

    // Create parent directory
    std::fs::create_dir_all(cache_path.parent().unwrap())
        .map_err(|e| anyhow!("Failed to create cache dir: {}", e))?;

    // Clone with depth=1 at specific tag
    let status = Command::new("git")
        .arg("clone")
        .arg("--depth=1")
        .arg("--branch")
        .arg(version)
        .arg(&repo_url)
        .arg(&cache_path)
        .status()
        .map_err(|e| anyhow!("Failed to run git clone: {}", e))?;

    if !status.success() {
        return Err(anyhow!(
            "Failed to clone templates from {} at tag {}",
            repo_url,
            version
        ));
    }

    // Remove .git to save space
    let git_dir = cache_path.join(".git");
    if git_dir.exists() {
        std::fs::remove_dir_all(&git_dir)
            .ok();
    }

    eprintln!(
        "✓ Templates cached to {}",
        cache_path.display()
    );
    Ok(cache_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_git_url() {
        assert_eq!(
            normalize_git_url("Owner/repo"),
            "https://github.com/Owner/repo.git"
        );
        assert_eq!(
            normalize_git_url("Owner/repo.git"),
            "https://github.com/Owner/repo.git"
        );
        assert_eq!(
            normalize_git_url("https://github.com/Owner/repo.git"),
            "https://github.com/Owner/repo.git"
        );
        assert_eq!(
            normalize_git_url("git@github.com:Owner/repo.git"),
            "git@github.com:Owner/repo.git"
        );
    }

    #[test]
    fn test_cache_dir() {
        let path = cache_dir("https://github.com/Rushit/tonin-templates-flat.git", "v0.4.0")
            .unwrap();
        assert!(path.to_string_lossy().contains("tonin-templates-flat"));
        assert!(path.to_string_lossy().contains("v0.4.0"));
    }
}
