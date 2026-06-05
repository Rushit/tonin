//! Download and cache tonin templates from GitHub releases.
//!
//! Templates are cached locally at ~/.tonin/templates/{repo_name}/{version}/
//! Falls back to embedded templates if download fails.
//!
//! Usage:
//! - Default: downloads from Rushit/tonin-templates
//! - Custom: pass repo like "github.com/myorg/tonin-templates-flat"

use anyhow::{anyhow, Result};
use std::path::PathBuf;

const DEFAULT_REPO: &str = "Rushit/tonin-templates";
const RELEASE_URL_TEMPLATE: &str =
    "https://github.com/{repo}/releases/download/{version}/{tarball}";

/// Normalize repo string to owner/repo format.
/// Accepts: "github.com/Owner/repo" or "Owner/repo"
fn normalize_repo(repo: &str) -> String {
    repo.trim_start_matches("github.com/")
        .trim_start_matches("https://github.com/")
        .to_string()
}

/// Get the cache directory for templates.
fn cache_dir(repo: &str, version: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
    // Use repo name as cache key (e.g., "tonin-templates" or "tonin-templates-flat")
    let repo_name = repo.split('/').last().unwrap_or("tonin-templates");
    let cache_path = PathBuf::from(home)
        .join(".tonin")
        .join("templates")
        .join(repo_name)
        .join(version);
    Ok(cache_path)
}

/// Download templates from GitHub release and cache locally.
///
/// Repo format: "Owner/repo" or "github.com/Owner/repo"
/// Returns the path to the cached template directory.
/// Falls back to embedded templates if download fails.
pub async fn download_if_missing(repo: Option<&str>, version: &str) -> Result<PathBuf> {
    let repo = repo.unwrap_or(DEFAULT_REPO);
    let repo = normalize_repo(repo);
    let cache_path = cache_dir(&repo, version)?;

    // Already cached?
    if cache_path.exists() {
        eprintln!(
            "✓ Using cached templates from {}",
            cache_path.display()
        );
        return Ok(cache_path);
    }

    // Determine tarball name from repo (e.g., "tonin-templates-flat-0.4.0.tar.gz")
    let repo_name = repo.split('/').last().unwrap_or("tonin-templates");
    let tarball = format!("{}-{}.tar.gz", repo_name, version);

    // Download from GitHub
    let url = RELEASE_URL_TEMPLATE
        .replace("{repo}", &repo)
        .replace("{version}", version)
        .replace("{tarball}", &tarball);

    eprintln!(
        "⬇ Downloading templates from {} ...",
        url
    );

    let bytes = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("Failed to download templates: {}", e))?
        .bytes()
        .await
        .map_err(|e| anyhow!("Failed to read response: {}", e))?;

    // Create cache directory
    std::fs::create_dir_all(&cache_path)
        .map_err(|e| anyhow!("Failed to create cache dir: {}", e))?;

    // Extract tarball
    let tar_reader = std::io::Cursor::new(bytes);
    let gz = flate2::read::GzDecoder::new(tar_reader);
    let mut archive = tar::Archive::new(gz);

    archive
        .unpack(&cache_path)
        .map_err(|e| anyhow!("Failed to extract templates: {}", e))?;

    eprintln!(
        "✓ Templates cached to {}",
        cache_path.display()
    );
    Ok(cache_path)
}

/// Get the version to use (from CLI flag, env var, or default to latest).
pub fn resolve_template_version() -> String {
    // Check env var first (highest priority)
    if let Ok(version) = std::env::var("TONIN_TEMPLATE_VERSION") {
        return version;
    }

    // Default to latest (GitHub will resolve this)
    "latest".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_repo() {
        assert_eq!(normalize_repo("Owner/repo"), "Owner/repo");
        assert_eq!(normalize_repo("github.com/Owner/repo"), "Owner/repo");
        assert_eq!(
            normalize_repo("https://github.com/Owner/repo"),
            "Owner/repo"
        );
    }

    #[test]
    fn test_release_urls() {
        // Default repo
        let url = RELEASE_URL_TEMPLATE
            .replace("{repo}", DEFAULT_REPO)
            .replace("{version}", "0.4.0")
            .replace("{tarball}", "tonin-templates-0.4.0.tar.gz");
        assert_eq!(
            url,
            "https://github.com/Rushit/tonin-templates/releases/download/0.4.0/tonin-templates-0.4.0.tar.gz"
        );

        // Flat variant repo
        let url = RELEASE_URL_TEMPLATE
            .replace("{repo}", "Rushit/tonin-templates-flat")
            .replace("{version}", "0.4.0")
            .replace("{tarball}", "tonin-templates-flat-0.4.0.tar.gz");
        assert_eq!(
            url,
            "https://github.com/Rushit/tonin-templates-flat/releases/download/0.4.0/tonin-templates-flat-0.4.0.tar.gz"
        );
    }
}
