//! etcd v3 implementation of the [`tonin_core::traits::Config`] trait.
//!
//! Use this when you want a real, push-based config backend with live
//! reload — etcd's watch API delivers changes to clients as soon as the
//! cluster commits them, no polling. Pair it with `EnvConfig` via
//! `ChainedConfig` for "etcd, fall back to env, fall back to defaults".
//!
//! ## Quick example
//!
//! ```no_run
//! use tonin_config_etcd::EtcdConfig;
//!
//! # async fn run() -> Result<(), tonin_core::Error> {
//! // From env vars (TONIN_CONFIG_ETCD_ENDPOINTS, TONIN_CONFIG_ETCD_PREFIX, ...).
//! let cfg = EtcdConfig::from_env().await?;
//!
//! // Or build manually:
//! let cfg = EtcdConfig::builder()
//!     .endpoint("https://etcd.kube-system.svc.cluster.local:2379")
//!     .prefix("/tonin/greeter/")
//!     .auth("svc-account", "hunter2")
//!     .connect()
//!     .await?;
//! # Ok(()) }
//! ```
//!
//! ## TLS
//!
//! Set `TONIN_CONFIG_ETCD_TLS_CA` to enable TLS with a custom root, plus
//! optional `TONIN_CONFIG_ETCD_TLS_CERT` + `TONIN_CONFIG_ETCD_TLS_KEY` for
//! mTLS to the etcd cluster. When any TLS env var is set, the client uses
//! TLS; otherwise it falls back to plaintext (suitable for in-cluster
//! traffic over a mesh).
//!
//! ## Watch semantics
//!
//! etcd is push-based — `watch` ignores the `interval` argument and emits
//! a new value as soon as the cluster delivers a `PUT` or `DELETE` event
//! for the watched key. The background task survives transient etcd
//! errors via exponential backoff (1s → 30s) and exits when the last
//! receiver is dropped.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use etcd_client::{Certificate, Client, ConnectOptions, GetOptions, Identity, TlsOptions};
use tokio::sync::watch;
use tonin_core::Error;
use tonin_core::traits::Config;

const ENV_ENDPOINTS: &str = "TONIN_CONFIG_ETCD_ENDPOINTS";
const ENV_PREFIX: &str = "TONIN_CONFIG_ETCD_PREFIX";
const ENV_TLS_CA: &str = "TONIN_CONFIG_ETCD_TLS_CA";
const ENV_TLS_CERT: &str = "TONIN_CONFIG_ETCD_TLS_CERT";
const ENV_TLS_KEY: &str = "TONIN_CONFIG_ETCD_TLS_KEY";
const ENV_USER: &str = "TONIN_CONFIG_ETCD_USER";
const ENV_PASSWORD: &str = "TONIN_CONFIG_ETCD_PASSWORD";

/// Materialized config backend talking to etcd v3.
///
/// Cheap to clone — the underlying `etcd_client::Client` is `Clone` and
/// shares its connection pool across clones. Wrap in an `Arc` if you want
/// to share across multiple owners without leaning on the internal
/// reference counting.
#[derive(Clone)]
pub struct EtcdConfig {
    client: Client,
    key_prefix: String,
}

/// Builder for [`EtcdConfig`]. Construct with [`EtcdConfig::builder`].
///
/// All fields are optional except `endpoints`. The default endpoint set is
/// empty, so calling [`Self::connect`] without endpoints returns a config
/// error.
#[derive(Default, Clone)]
pub struct EtcdConfigBuilder {
    endpoints: Vec<String>,
    key_prefix: String,
    tls: Option<TlsConfig>,
    auth: Option<(String, String)>,
}

/// TLS material paths for connecting to etcd over TLS / mTLS.
///
/// - `ca_path`: PEM-encoded CA cert to verify the etcd server's cert
///   against. Required for TLS.
/// - `cert_path` + `key_path`: PEM-encoded client cert + private key for
///   mTLS. Both must be set together; supplying only one is an error.
#[derive(Default, Clone, Debug)]
pub struct TlsConfig {
    pub ca_path: Option<PathBuf>,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
}

impl TlsConfig {
    fn is_empty(&self) -> bool {
        self.ca_path.is_none() && self.cert_path.is_none() && self.key_path.is_none()
    }
}

impl EtcdConfig {
    /// Start a new builder. See [`EtcdConfigBuilder`].
    pub fn builder() -> EtcdConfigBuilder {
        EtcdConfigBuilder::default()
    }

    /// Build an `EtcdConfig` from `TONIN_CONFIG_ETCD_*` env vars.
    ///
    /// Required:
    /// - `TONIN_CONFIG_ETCD_ENDPOINTS` — comma-separated list, e.g.
    ///   `"https://etcd-0:2379,https://etcd-1:2379"`.
    ///
    /// Optional:
    /// - `TONIN_CONFIG_ETCD_PREFIX` — prepended to every key path.
    /// - `TONIN_CONFIG_ETCD_TLS_CA` / `_CERT` / `_KEY` — paths to PEM files.
    /// - `TONIN_CONFIG_ETCD_USER` / `_PASSWORD` — basic auth.
    pub async fn from_env() -> Result<Self, Error> {
        let raw_endpoints = std::env::var(ENV_ENDPOINTS).map_err(|_| {
            Error::Config(format!(
                "{ENV_ENDPOINTS} is not set; required for etcd config backend"
            ))
        })?;
        let endpoints = parse_endpoints(&raw_endpoints);
        if endpoints.is_empty() {
            return Err(Error::Config(format!("{ENV_ENDPOINTS} is empty")));
        }

        let mut builder = EtcdConfigBuilder::default().endpoints(endpoints);

        if let Ok(prefix) = std::env::var(ENV_PREFIX) {
            builder = builder.prefix(prefix);
        }

        let ca = std::env::var(ENV_TLS_CA).ok().map(PathBuf::from);
        let cert = std::env::var(ENV_TLS_CERT).ok().map(PathBuf::from);
        let key = std::env::var(ENV_TLS_KEY).ok().map(PathBuf::from);
        if ca.is_some() || cert.is_some() || key.is_some() {
            builder = builder.tls(TlsConfig {
                ca_path: ca,
                cert_path: cert,
                key_path: key,
            });
        }

        if let (Ok(user), Ok(password)) = (std::env::var(ENV_USER), std::env::var(ENV_PASSWORD)) {
            builder = builder.auth(user, password);
        }

        builder.connect().await
    }
}

impl EtcdConfigBuilder {
    /// Add a single etcd endpoint. May be called multiple times.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoints.push(endpoint.into());
        self
    }

    /// Replace all endpoints in one shot.
    pub fn endpoints(mut self, endpoints: impl IntoIterator<Item = String>) -> Self {
        self.endpoints = endpoints.into_iter().collect();
        self
    }

    /// Prefix prepended to every key path passed to `get` / `watch`.
    ///
    /// Example: with `prefix = "/tonin/greeter/"`, calling
    /// `cfg.get("db.pool.max")` reads the etcd key
    /// `/tonin/greeter/db.pool.max`.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Enable TLS using the supplied PEM file paths. See [`TlsConfig`].
    pub fn tls(mut self, tls: TlsConfig) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Set basic-auth credentials for etcd.
    pub fn auth(mut self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.auth = Some((user.into(), password.into()));
        self
    }

    /// Open the etcd connection and return the [`EtcdConfig`].
    ///
    /// Returns `Error::Config` for invalid setup (no endpoints, partial
    /// mTLS material, unreadable PEM file) and `Error::CapabilityTransient`
    /// if the cluster is unreachable at connect time.
    pub async fn connect(self) -> Result<EtcdConfig, Error> {
        if self.endpoints.is_empty() {
            return Err(Error::Config(
                "etcd config: at least one endpoint is required".into(),
            ));
        }

        let mut options = ConnectOptions::new();

        if let Some(tls) = &self.tls
            && !tls.is_empty()
        {
            let tls_options = build_tls_options(tls)?;
            options = options.with_tls(tls_options);
        }

        if let Some((user, password)) = &self.auth {
            options = options.with_user(user.clone(), password.clone());
        }

        let client = Client::connect(&self.endpoints, Some(options))
            .await
            .map_err(|e| {
                Error::CapabilityTransient(format!(
                    "etcd connect to {:?} failed: {e}",
                    self.endpoints
                ))
            })?;

        Ok(EtcdConfig {
            client,
            key_prefix: self.key_prefix,
        })
    }
}

fn build_tls_options(tls: &TlsConfig) -> Result<TlsOptions, Error> {
    let mut opts = TlsOptions::new();

    if let Some(ca) = &tls.ca_path {
        let pem = std::fs::read(ca)
            .map_err(|e| Error::Config(format!("read CA cert {}: {e}", ca.display())))?;
        opts = opts.ca_certificate(Certificate::from_pem(pem));
    }

    match (&tls.cert_path, &tls.key_path) {
        (Some(cert), Some(key)) => {
            let cert_pem = std::fs::read(cert)
                .map_err(|e| Error::Config(format!("read client cert {}: {e}", cert.display())))?;
            let key_pem = std::fs::read(key)
                .map_err(|e| Error::Config(format!("read client key {}: {e}", key.display())))?;
            opts = opts.identity(Identity::from_pem(cert_pem, key_pem));
        }
        (None, None) => {}
        _ => {
            return Err(Error::Config(
                "etcd TLS: cert and key must be supplied together for mTLS".into(),
            ));
        }
    }

    Ok(opts)
}

fn parse_endpoints(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn full_key(prefix: &str, path: &str) -> String {
    let mut s = String::with_capacity(prefix.len() + path.len());
    s.push_str(prefix);
    s.push_str(path);
    s
}

fn validate_path(path: &str) -> Result<(), Error> {
    if path.is_empty() {
        return Err(Error::CapabilityPermanent(
            "config path must not be empty".into(),
        ));
    }
    Ok(())
}

fn classify_etcd_error(action: &str, key: &str, err: etcd_client::Error) -> Error {
    use etcd_client::Error as E;
    match err {
        E::InvalidArgs(msg) => {
            Error::CapabilityPermanent(format!("etcd {action} '{key}': invalid args: {msg}"))
        }
        E::InvalidUri(uri) => {
            Error::CapabilityPermanent(format!("etcd {action} '{key}': invalid uri: {uri}"))
        }
        E::Utf8Error(e) => {
            Error::CapabilityPermanent(format!("etcd {action} '{key}': utf8 error: {e}"))
        }
        E::InvalidHeaderValue(e) => {
            Error::CapabilityPermanent(format!("etcd {action} '{key}': bad header: {e}"))
        }
        other => Error::CapabilityTransient(format!("etcd {action} '{key}': {other}")),
    }
}

#[async_trait]
impl Config for EtcdConfig {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        validate_path(path)?;
        let key = full_key(&self.key_prefix, path);
        // Client is Clone (Arc internally) — get/watch need &mut, so we
        // clone per call. Cheap; just bumps a refcount.
        let mut client = self.client.clone();
        let resp = client
            .get(key.clone(), None)
            .await
            .map_err(|e| classify_etcd_error("get", &key, e))?;
        Ok(resp.kvs().first().map(|kv| kv.value().to_vec()))
    }

    fn watch(&self, path: &str, _interval: Duration) -> watch::Receiver<Option<Vec<u8>>> {
        // Empty path -> emit None forever; logging keeps the failure
        // visible without crashing the service.
        if let Err(e) = validate_path(path) {
            tracing::warn!(error = %e, "etcd config watch called with invalid path");
            let (_tx, rx) = watch::channel(None);
            return rx;
        }

        let key = full_key(&self.key_prefix, path);
        let client = self.client.clone();
        let (tx, rx) = watch::channel(None);
        let tx = Arc::new(tx);

        tokio::spawn(watch_loop(client, key, tx));

        rx
    }

    fn source(&self) -> &'static str {
        "etcd"
    }
}

async fn watch_loop(client: Client, key: String, tx: Arc<watch::Sender<Option<Vec<u8>>>>) {
    // Backoff schedule for the outer reconnect loop. We keep it bounded
    // so a long etcd outage doesn't peg CPU but also doesn't slow recovery
    // once the cluster is back.
    let backoff_steps: [Duration; 6] = [
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(8),
        Duration::from_secs(16),
        Duration::from_secs(30),
    ];
    let mut attempt: usize = 0;

    loop {
        if tx.is_closed() {
            return;
        }

        match run_watch_session(client.clone(), &key, &tx).await {
            Ok(()) => return, // receiver dropped
            Err(e) => {
                let delay = backoff_steps[attempt.min(backoff_steps.len() - 1)];
                tracing::warn!(
                    key = %key,
                    error = %e,
                    backoff_secs = delay.as_secs(),
                    "etcd config watch errored; reconnecting after backoff",
                );
                attempt = attempt.saturating_add(1);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = tx.closed() => return,
                }
            }
        }
    }
}

async fn run_watch_session(
    mut client: Client,
    key: &str,
    tx: &watch::Sender<Option<Vec<u8>>>,
) -> Result<(), etcd_client::Error> {
    // Seed the channel with the current value before subscribing — the
    // contract says receivers see the current value at subscribe time.
    let current = client
        .get(key.as_bytes().to_vec(), Some(GetOptions::new()))
        .await?
        .kvs()
        .first()
        .map(|kv| kv.value().to_vec());
    if tx.send(current).is_err() {
        // No receivers — bail out, drop the watcher, free the connection.
        return Ok(());
    }

    let (_watcher, mut stream) = client.watch(key.as_bytes().to_vec(), None).await?;

    loop {
        tokio::select! {
            _ = tx.closed() => return Ok(()),
            msg = stream.message() => {
                let resp = match msg? {
                    Some(r) => r,
                    None => return Ok(()),
                };
                for event in resp.events() {
                    let new_value = match event.event_type() {
                        etcd_client::EventType::Put => event.kv().map(|kv| kv.value().to_vec()),
                        etcd_client::EventType::Delete => None,
                    };
                    if tx.send(new_value).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoints_splits_and_trims() {
        let v = parse_endpoints("https://a:2379, https://b:2379 ,,https://c:2379");
        assert_eq!(
            v,
            vec![
                "https://a:2379".to_string(),
                "https://b:2379".to_string(),
                "https://c:2379".to_string()
            ]
        );
    }

    #[test]
    fn parse_endpoints_empty_yields_empty() {
        assert!(parse_endpoints("").is_empty());
        assert!(parse_endpoints("   ,  ").is_empty());
    }

    #[test]
    fn full_key_concatenates_prefix() {
        assert_eq!(full_key("/tonin/", "db.pool.max"), "/tonin/db.pool.max");
        assert_eq!(full_key("", "db.pool.max"), "db.pool.max");
    }

    #[test]
    fn validate_path_rejects_empty() {
        assert!(validate_path("").is_err());
        assert!(validate_path("ok").is_ok());
    }

    #[test]
    fn builder_collects_fields() {
        let b = EtcdConfig::builder()
            .endpoint("https://a:2379")
            .endpoint("https://b:2379")
            .prefix("/svc/")
            .auth("u", "p");
        assert_eq!(b.endpoints.len(), 2);
        assert_eq!(b.key_prefix, "/svc/");
        assert_eq!(b.auth, Some(("u".into(), "p".into())));
        assert!(b.tls.is_none());
    }

    #[test]
    fn builder_endpoints_replaces() {
        let b = EtcdConfig::builder()
            .endpoint("https://old:2379")
            .endpoints(vec!["https://new:2379".to_string()]);
        assert_eq!(b.endpoints, vec!["https://new:2379".to_string()]);
    }

    #[tokio::test]
    async fn connect_with_no_endpoints_is_config_error() {
        // EtcdConfig deliberately has no Debug impl (the inner etcd client
        // doesn't expose one). Match by hand instead of unwrap_err().
        match EtcdConfig::builder().connect().await {
            Err(Error::Config(msg)) => assert!(msg.contains("endpoint")),
            Err(other) => panic!("expected Error::Config, got {other}"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
    }

    #[test]
    fn tls_is_empty_when_all_paths_none() {
        let t = TlsConfig::default();
        assert!(t.is_empty());
        let t = TlsConfig {
            ca_path: Some(PathBuf::from("/x")),
            ..Default::default()
        };
        assert!(!t.is_empty());
    }

    #[test]
    fn build_tls_options_rejects_partial_mtls() {
        let only_cert = TlsConfig {
            ca_path: None,
            cert_path: Some(PathBuf::from("/does/not/exist/cert.pem")),
            key_path: None,
        };
        let err = build_tls_options(&only_cert).unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("cert and key")),
            other => panic!("expected Error::Config, got {other}"),
        }
    }
}
