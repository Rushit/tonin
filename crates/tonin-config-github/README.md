# tonin-config-github

GitOps-style application config for [tonin](https://crates.io/crates/tonin) services: read config files from a private GitHub repo over the Contents API, with poll-based hot reload driven by the head commit SHA.

Part of the [tonin](https://crates.io/crates/tonin) framework.

## When to use

Reach for this engine when the same git repo that holds your k8s manifests, Helm values, or deployment overlays also owns your application config — feature flags, DB pool sizes, tuning knobs — and you want one PR-reviewed source of truth for "what's running where". Pair with [`tonin-core::ChainedConfig`](https://docs.rs/tonin-core) and `EnvConfig` if you want env overrides on top.

Not the right tool for secrets — use a `SecretStore` impl for credentials. Use [`tonin-config-etcd`](https://crates.io/crates/tonin-config-etcd) when you want server-pushed updates instead of poll.

## Example

```rust,no_run
use std::sync::Arc;
use std::time::Duration;
use tonin_core::traits::Config;
use tonin_config_github::GithubConfig;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let cfg: Arc<dyn Config> = Arc::new(GithubConfig::from_env()?);

// One-shot read.
let bytes = cfg.get("services/orders/pool.json").await?;

// Watch for changes — polls the head SHA every 30s.
let mut rx = cfg.watch("services/orders/pool.json", Duration::from_secs(30));
while rx.changed().await.is_ok() {
    let snapshot = rx.borrow().clone();
    // reload your in-memory config from `snapshot`
}
# Ok(()) }
```

Configure via env:

```sh
TONIN_CONFIG_GITHUB_REPO=acme/platform-config
TONIN_CONFIG_GITHUB_REF=main                       # optional, default "main"
TONIN_CONFIG_GITHUB_PATH_PREFIX=services/orders/   # optional, default ""
TONIN_CONFIG_GITHUB_TOKEN=ghp_xxx                  # PAT or installation token
```

## Auth

The `Authorization: Bearer <token>` header carries either a **personal access token** (PAT — fine-grained or classic, scope `repo` for private repos) or a **GitHub App installation token** (mint short-lived tokens server-side from a JWT signed with your App's private key).

Prefer GitHub App tokens in production: they're short-lived, scoped to one installation, and revocable independently of any human user. PATs are fine for local development and one-off scripts.

The token is never logged. Failed auth (`401`/`403`) surfaces as `Error::CapabilityPermanent` so retry loops fail fast instead of hammering the API.

## Status

Pre-alpha. The trait surface (`Config`) is stable; the `from_env` env var names and exact retry/backoff behavior of `watch` may evolve. See [`docs/roadmap.md`](https://github.com/Rushit/tonin/blob/main/docs/roadmap.md) for the broader Config / SecretStore plan.

---

Licensed under the Apache License, Version 2.0.
