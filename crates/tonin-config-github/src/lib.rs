//! GitHub-repo-backed [`Config`] implementation for the tonin framework.
//!
//! Reads application config files from a private GitHub repo via the
//! [Contents API](https://docs.github.com/en/rest/repos/contents). Hot
//! reload is implemented by polling the [head commit
//! SHA](https://docs.github.com/en/rest/commits/commits#get-a-commit) of
//! the configured ref on a caller-supplied interval; when the SHA moves,
//! affected `watch` subscribers re-fetch and emit.
//!
//! See the crate-level [README](https://github.com/Rushit/tonin/tree/main/crates/tonin-config-github)
//! for the bigger picture and auth notes.
//!
//! # Quick start
//!
//! ```no_run
//! use tonin_sdk::traits::Config;
//! use tonin_config_github::GithubConfig;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = GithubConfig::from_env()?;
//! let bytes = cfg.get("pool.json").await?;
//! # Ok(()) }
//! ```

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use tokio::sync::RwLock;
use tonin_sdk::Error;
use tonin_sdk::traits::Config;

/// User-Agent string sent on every API call. GitHub rejects requests
/// without a UA, so we always set one — version pulled from cargo.
const USER_AGENT: &str = concat!("tonin-config-github/", env!("CARGO_PKG_VERSION"));

/// Default git ref used when none is supplied.
const DEFAULT_REF: &str = "main";

/// Cap the exponential backoff between failed poll attempts. Without
/// this, a long outage drives the poll interval into the multi-hour
/// range and the first request after recovery is needlessly delayed.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Env var names — kept as constants so the unit tests can assert
/// the same strings the production constructor reads.
const ENV_REPO: &str = "TONIN_CONFIG_GITHUB_REPO";
const ENV_REF: &str = "TONIN_CONFIG_GITHUB_REF";
const ENV_PATH_PREFIX: &str = "TONIN_CONFIG_GITHUB_PATH_PREFIX";
const ENV_TOKEN: &str = "TONIN_CONFIG_GITHUB_TOKEN";

/// JSON shape of GitHub's Contents API response (for files).
///
/// The API also returns directory listings as a JSON array; we treat
/// those as "not a file" and surface a permanent error, since a config
/// path that resolves to a directory is a caller bug.
#[derive(Debug, Deserialize)]
struct ContentsResponse {
    /// `"base64"` for files. Directory responses don't include this
    /// field at all (they're an array, not an object) so we never see
    /// `None` here in practice — but we tolerate it defensively.
    #[serde(default)]
    encoding: Option<String>,
    /// Base64-encoded file body. GitHub wraps lines at 60 chars, so
    /// strip whitespace before decoding.
    #[serde(default)]
    content: Option<String>,
}

/// JSON shape of GitHub's "Get a commit" response. We only need the
/// `sha` field — everything else is ignored.
#[derive(Debug, Deserialize)]
struct CommitResponse {
    sha: String,
}

/// `Config` implementation backed by a GitHub repo.
///
/// Construct via [`GithubConfig::builder`] or [`GithubConfig::from_env`].
pub struct GithubConfig {
    client: reqwest::Client,
    /// `"owner/name"`.
    repo: String,
    /// Branch, tag, or commit SHA. Defaults to `"main"`.
    git_ref: String,
    /// Optional path prefix prepended to every `get` key, e.g.
    /// `"services/myservice/"`. Empty means "no prefix".
    path_prefix: String,
    /// Bearer token. Not exposed publicly; never logged. Plain `String`
    /// rather than a wrapped secret type to avoid pulling a crate in.
    token: String,
    /// Last observed head SHA, populated by `watch` poll loops. Wrapped
    /// in an `RwLock` so concurrent watchers can share the cache.
    last_sha: RwLock<Option<String>>,
}

/// Builder for [`GithubConfig`].
#[derive(Default)]
pub struct GithubConfigBuilder {
    repo: Option<String>,
    git_ref: String,
    path_prefix: String,
    token: Option<String>,
}

impl GithubConfigBuilder {
    /// Start a new builder with default ref `"main"` and no path prefix.
    pub fn new() -> Self {
        Self {
            repo: None,
            git_ref: DEFAULT_REF.to_string(),
            path_prefix: String::new(),
            token: None,
        }
    }

    /// Set the repo, formatted `"owner/name"`. Required.
    pub fn repo(mut self, repo: impl Into<String>) -> Self {
        self.repo = Some(repo.into());
        self
    }

    /// Set the git ref (branch, tag, or commit SHA). Defaults to `"main"`.
    pub fn git_ref(mut self, git_ref: impl Into<String>) -> Self {
        self.git_ref = git_ref.into();
        self
    }

    /// Set an optional path prefix prepended to every `get` key.
    pub fn path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.path_prefix = prefix.into();
        self
    }

    /// Set the bearer token (PAT or GitHub App installation token).
    /// Required.
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Build the [`GithubConfig`]. Fails with [`Error::Config`] if any
    /// required field is missing or the underlying HTTP client cannot
    /// be constructed.
    pub fn build(self) -> Result<GithubConfig, Error> {
        let repo = self
            .repo
            .ok_or_else(|| Error::Config("github config: repo is required".into()))?;
        if !repo.contains('/') {
            return Err(Error::Config(format!(
                "github config: repo '{repo}' must be in 'owner/name' form"
            )));
        }
        let token = self
            .token
            .ok_or_else(|| Error::Config("github config: token is required".into()))?;

        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| Error::Config(format!("github config: build http client: {e}")))?;

        let git_ref = if self.git_ref.is_empty() {
            DEFAULT_REF.to_string()
        } else {
            self.git_ref
        };

        Ok(GithubConfig {
            client,
            repo,
            git_ref,
            path_prefix: self.path_prefix,
            token,
            last_sha: RwLock::new(None),
        })
    }
}

impl GithubConfig {
    /// Start a builder.
    pub fn builder() -> GithubConfigBuilder {
        GithubConfigBuilder::new()
    }

    /// Construct from the standard `TONIN_CONFIG_GITHUB_*` env vars.
    ///
    /// - `TONIN_CONFIG_GITHUB_REPO` — required, `"owner/name"`.
    /// - `TONIN_CONFIG_GITHUB_REF` — optional, defaults to `"main"`.
    /// - `TONIN_CONFIG_GITHUB_PATH_PREFIX` — optional, defaults to `""`.
    /// - `TONIN_CONFIG_GITHUB_TOKEN` — required, PAT or installation token.
    pub fn from_env() -> Result<Self, Error> {
        let repo = std::env::var(ENV_REPO)
            .map_err(|_| Error::Config(format!("github config: ${ENV_REPO} is not set")))?;
        let token = std::env::var(ENV_TOKEN)
            .map_err(|_| Error::Config(format!("github config: ${ENV_TOKEN} is not set")))?;
        let git_ref = std::env::var(ENV_REF).unwrap_or_else(|_| DEFAULT_REF.to_string());
        let path_prefix = std::env::var(ENV_PATH_PREFIX).unwrap_or_default();

        GithubConfigBuilder::new()
            .repo(repo)
            .token(token)
            .git_ref(git_ref)
            .path_prefix(path_prefix)
            .build()
    }

    /// Build the `https://api.github.com/repos/<repo>/contents/<full_path>?ref=<git_ref>`
    /// URL for a given config path. Public-but-undocumented hook so the
    /// unit tests can assert the exact wire URL without firing a request.
    fn contents_url(&self, path: &str) -> String {
        let full_path = join_path(&self.path_prefix, path);
        format!(
            "https://api.github.com/repos/{}/contents/{}?ref={}",
            self.repo, full_path, self.git_ref
        )
    }

    /// Build the head-commit URL used by `watch` to detect changes.
    fn commit_url(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/commits/{}",
            self.repo, self.git_ref
        )
    }

    /// Issue the underlying Contents API call. Split out from the trait
    /// method so internal callers (the watch loop) can reuse it without
    /// going through `&dyn Config`.
    async fn fetch_contents(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        let url = self.contents_url(path);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::CapabilityTransient(format!("github contents request: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::CapabilityPermanent(format!(
                "github contents {url}: {status}"
            )));
        }
        if status.is_server_error() {
            return Err(Error::CapabilityTransient(format!(
                "github contents {url}: {status}"
            )));
        }
        if !status.is_success() {
            // 4xx that isn't 401/403/404 — treat as permanent (bad ref, etc.)
            return Err(Error::CapabilityPermanent(format!(
                "github contents {url}: {status}"
            )));
        }

        let body: ContentsResponse = resp.json().await.map_err(|e| {
            Error::CapabilityPermanent(format!("github contents {url}: decode json: {e}"))
        })?;

        let encoding = body.encoding.as_deref().unwrap_or("base64");
        if encoding != "base64" {
            return Err(Error::CapabilityPermanent(format!(
                "github contents {url}: unsupported encoding '{encoding}'"
            )));
        }
        let raw = body.content.ok_or_else(|| {
            Error::CapabilityPermanent(format!(
                "github contents {url}: missing 'content' field (is this a directory?)"
            ))
        })?;
        // GitHub wraps the base64 payload at 60 chars with \n. Strip
        // all ASCII whitespace before decoding — base64 doesn't accept
        // it, and the wrap width is GitHub's choice, not ours.
        let cleaned: String = raw.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let bytes = BASE64.decode(cleaned.as_bytes()).map_err(|e| {
            Error::CapabilityPermanent(format!("github contents {url}: base64 decode: {e}"))
        })?;
        Ok(Some(bytes))
    }

    /// Fetch the current head SHA of `git_ref`. Returns the SHA on
    /// success; `None` if the ref is missing (404).
    async fn fetch_head_sha(&self) -> Result<Option<String>, Error> {
        let url = self.commit_url();
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::CapabilityTransient(format!("github commit request: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::CapabilityPermanent(format!(
                "github commit {url}: {status}"
            )));
        }
        if status.is_server_error() {
            return Err(Error::CapabilityTransient(format!(
                "github commit {url}: {status}"
            )));
        }
        if !status.is_success() {
            return Err(Error::CapabilityPermanent(format!(
                "github commit {url}: {status}"
            )));
        }
        let body: CommitResponse = resp.json().await.map_err(|e| {
            Error::CapabilityPermanent(format!("github commit {url}: decode json: {e}"))
        })?;
        Ok(Some(body.sha))
    }
}

#[async_trait]
impl Config for GithubConfig {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        self.fetch_contents(path).await
    }

    fn watch(
        &self,
        path: &str,
        interval: Duration,
    ) -> tokio::sync::watch::Receiver<Option<Vec<u8>>> {
        // Snapshot everything the poll task needs so it doesn't hold a
        // reference to `&self` (the task outlives the `&self` borrow).
        let client = self.client.clone();
        let repo = self.repo.clone();
        let git_ref = self.git_ref.clone();
        let path_prefix = self.path_prefix.clone();
        let token = self.token.clone();
        let path = path.to_string();

        let (tx, rx) = tokio::sync::watch::channel::<Option<Vec<u8>>>(None);

        tokio::spawn(async move {
            // Re-construct a lightweight clone of GithubConfig with a
            // fresh per-task SHA cache. We can't share `self.last_sha`
            // across watchers cleanly through a snapshot, and per-watcher
            // tracking is the right granularity anyway: a watcher only
            // cares about transitions it has personally observed.
            let inner = GithubConfig {
                client,
                repo,
                git_ref,
                path_prefix,
                token,
                last_sha: RwLock::new(None),
            };

            // Initial fetch: emit immediately so subscribers see the
            // current value at subscribe time, matching the contract
            // documented on `Config::watch`.
            match inner.fetch_contents(&path).await {
                Ok(bytes) => {
                    // `send` only fails if all receivers dropped, in
                    // which case we shut down — same handling as below.
                    if tx.send(bytes).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path,
                        error = %e,
                        "github config initial fetch failed; watcher will retry",
                    );
                }
            }

            let mut current_interval = interval;
            let mut consecutive_failures: u32 = 0;

            loop {
                // Honor receiver lifecycle: if everyone's gone, exit.
                if tx.is_closed() {
                    return;
                }

                tokio::select! {
                    _ = tokio::time::sleep(current_interval) => {}
                    _ = tx.closed() => return,
                }

                match inner.fetch_head_sha().await {
                    Ok(Some(sha)) => {
                        consecutive_failures = 0;
                        current_interval = interval;

                        let last = inner.last_sha.read().await.clone();
                        if last.as_deref() == Some(sha.as_str()) {
                            // No change; keep waiting.
                            continue;
                        }
                        // SHA moved — refetch the watched path.
                        match inner.fetch_contents(&path).await {
                            Ok(bytes) => {
                                *inner.last_sha.write().await = Some(sha);
                                if tx.send(bytes).is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    path = %path,
                                    error = %e,
                                    "github config refetch failed after SHA change",
                                );
                                // Don't update `last_sha` — we want to
                                // retry the fetch on the next tick.
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::warn!(
                            ref_ = %inner.git_ref,
                            "github config head SHA poll: ref not found (404); will keep polling",
                        );
                        // Treat as transient for backoff purposes —
                        // the ref might appear later (branch created).
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        current_interval = backoff(interval, consecutive_failures);
                    }
                    Err(Error::CapabilityPermanent(msg)) => {
                        tracing::error!(
                            error = %msg,
                            "github config head SHA poll: permanent error; stopping watcher",
                        );
                        return;
                    }
                    Err(e) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        current_interval = backoff(interval, consecutive_failures);
                        tracing::warn!(
                            error = %e,
                            attempts = consecutive_failures,
                            next_in = ?current_interval,
                            "github config head SHA poll failed transiently; backing off",
                        );
                    }
                }
            }
        });

        rx
    }

    fn source(&self) -> &'static str {
        "github"
    }
}

/// Exponential backoff: double the interval per consecutive failure,
/// capped at [`MAX_BACKOFF`]. `attempts == 1` returns `base * 2`,
/// `attempts == 2` returns `base * 4`, etc.
fn backoff(base: Duration, attempts: u32) -> Duration {
    // Saturating shift so very large counters don't panic. Clamp to
    // MAX_BACKOFF either way.
    let mult: u32 = 1u32.checked_shl(attempts).unwrap_or(u32::MAX);
    let scaled = base.saturating_mul(mult);
    if scaled > MAX_BACKOFF {
        MAX_BACKOFF
    } else {
        scaled
    }
}

/// Join `path_prefix` and `path` into a single API path segment,
/// avoiding double slashes regardless of trailing/leading slashes in
/// either input. Empty prefix is allowed.
fn join_path(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if prefix.is_empty() {
        path.to_string()
    } else {
        format!("{prefix}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cfg(prefix: &str, git_ref: &str) -> GithubConfig {
        GithubConfigBuilder::new()
            .repo("acme/platform")
            .token("ghp_dummy")
            .git_ref(git_ref)
            .path_prefix(prefix)
            .build()
            .expect("builder")
    }

    #[test]
    fn join_path_handles_empty_prefix() {
        assert_eq!(join_path("", "foo.json"), "foo.json");
        assert_eq!(join_path("", "/foo.json"), "foo.json");
    }

    #[test]
    fn join_path_normalizes_slashes() {
        assert_eq!(join_path("services/a", "foo.json"), "services/a/foo.json");
        assert_eq!(join_path("services/a/", "foo.json"), "services/a/foo.json");
        assert_eq!(join_path("services/a/", "/foo.json"), "services/a/foo.json");
        assert_eq!(join_path("services/a", "/foo.json"), "services/a/foo.json");
    }

    #[test]
    fn contents_url_uses_prefix_and_ref() {
        let cfg = make_cfg("services/orders/", "release");
        assert_eq!(
            cfg.contents_url("pool.json"),
            "https://api.github.com/repos/acme/platform/contents/services/orders/pool.json?ref=release"
        );
    }

    #[test]
    fn contents_url_with_no_prefix() {
        let cfg = make_cfg("", "main");
        assert_eq!(
            cfg.contents_url("pool.json"),
            "https://api.github.com/repos/acme/platform/contents/pool.json?ref=main"
        );
    }

    #[test]
    fn contents_url_strips_leading_slash() {
        let cfg = make_cfg("", "main");
        assert_eq!(
            cfg.contents_url("/pool.json"),
            "https://api.github.com/repos/acme/platform/contents/pool.json?ref=main"
        );
    }

    #[test]
    fn commit_url_targets_configured_ref() {
        let cfg = make_cfg("services/orders/", "release");
        assert_eq!(
            cfg.commit_url(),
            "https://api.github.com/repos/acme/platform/commits/release"
        );
    }

    #[test]
    fn builder_requires_repo() {
        // Avoid `expect_err` here — `GithubConfig` deliberately does
        // not implement `Debug` (would risk leaking the token), so we
        // pattern-match the result instead.
        match GithubConfigBuilder::new().token("t").build() {
            Err(Error::Config(msg)) => assert!(msg.contains("repo")),
            Err(other) => panic!("expected Error::Config, got {other:?}"),
            Ok(_) => panic!("expected an error when repo is missing"),
        }
    }

    #[test]
    fn builder_requires_token() {
        match GithubConfigBuilder::new().repo("o/r").build() {
            Err(Error::Config(msg)) => assert!(msg.contains("token")),
            Err(other) => panic!("expected Error::Config, got {other:?}"),
            Ok(_) => panic!("expected an error when token is missing"),
        }
    }

    #[test]
    fn builder_rejects_repo_without_slash() {
        match GithubConfigBuilder::new()
            .repo("just-a-name")
            .token("t")
            .build()
        {
            Err(Error::Config(msg)) => assert!(msg.contains("owner/name")),
            Err(other) => panic!("expected Error::Config, got {other:?}"),
            Ok(_) => panic!("expected an error for malformed repo"),
        }
    }

    #[test]
    fn builder_defaults_ref_to_main_when_empty() {
        // Explicitly clearing the ref via `git_ref("")` should fall
        // back to the default — matches `from_env` semantics when the
        // env var is set to an empty string.
        let cfg = GithubConfigBuilder::new()
            .repo("o/r")
            .token("t")
            .git_ref("")
            .build()
            .expect("builder");
        assert_eq!(cfg.git_ref, DEFAULT_REF);
    }

    #[test]
    fn source_is_github() {
        let cfg = make_cfg("", "main");
        assert_eq!(cfg.source(), "github");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let base = Duration::from_secs(30);
        assert_eq!(backoff(base, 1), Duration::from_secs(60));
        assert_eq!(backoff(base, 2), Duration::from_secs(120));
        // Eventually saturates at MAX_BACKOFF.
        let huge = backoff(base, 20);
        assert!(huge <= MAX_BACKOFF);
        assert_eq!(huge, MAX_BACKOFF);
    }

    #[test]
    fn user_agent_includes_crate_version() {
        // Sanity-check the const so the compile-time concat doesn't
        // drift if the package is renamed.
        assert!(USER_AGENT.starts_with("tonin-config-github/"));
        assert!(USER_AGENT.len() > "tonin-config-github/".len());
    }

    #[test]
    fn env_var_names_are_the_documented_ones() {
        // Pin the exact env var names — the README and the docs above
        // promise these strings, and renaming them is a breaking change
        // that should require touching this test on purpose.
        assert_eq!(ENV_REPO, "TONIN_CONFIG_GITHUB_REPO");
        assert_eq!(ENV_REF, "TONIN_CONFIG_GITHUB_REF");
        assert_eq!(ENV_PATH_PREFIX, "TONIN_CONFIG_GITHUB_PATH_PREFIX");
        assert_eq!(ENV_TOKEN, "TONIN_CONFIG_GITHUB_TOKEN");
    }
}
