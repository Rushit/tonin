//! Integration test against a real GitHub repo. Ignored by default —
//! requires `TONIN_CONFIG_GITHUB_REPO` and `TONIN_CONFIG_GITHUB_TOKEN`
//! to be set in the environment.
//!
//! Run with:
//!
//! ```sh
//! TONIN_CONFIG_GITHUB_REPO=owner/name \
//! TONIN_CONFIG_GITHUB_TOKEN=ghp_xxx \
//! TONIN_CONFIG_GITHUB_PATH_PREFIX=path/to/configs/ \
//! cargo test -p tonin-config-github --test integration_real_github -- --ignored --nocapture
//! ```

use tonin_config_github::GithubConfig;
use tonin_sdk::traits::Config;

#[tokio::test]
#[ignore = "requires a real GitHub repo + PAT"]
async fn from_env_fetches_a_known_path() {
    // The path defaults to README.md, which most repos have. Override
    // with TONIN_CONFIG_GITHUB_TEST_PATH if your repo doesn't.
    let path =
        std::env::var("TONIN_CONFIG_GITHUB_TEST_PATH").unwrap_or_else(|_| "README.md".to_string());

    let cfg = GithubConfig::from_env().expect("from_env");
    let bytes = cfg
        .get(&path)
        .await
        .expect("get must succeed against a real repo");
    let bytes = bytes.expect("file should exist at the configured path");
    assert!(
        !bytes.is_empty(),
        "fetched body should be non-empty (got {} bytes)",
        bytes.len()
    );
    assert_eq!(cfg.source(), "github");
}

#[tokio::test]
#[ignore = "requires a real GitHub repo + PAT"]
async fn missing_path_returns_none() {
    let cfg = GithubConfig::from_env().expect("from_env");
    let bytes = cfg
        .get("definitely/not/a/real/path/in/this/repo-zzz.json")
        .await
        .expect("404 should map to Ok(None), not Err");
    assert!(bytes.is_none(), "expected None for a missing path");
}
