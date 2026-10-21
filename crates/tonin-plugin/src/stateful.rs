//! Stateful dependencies: `[database]`, `[cache]`, `[secrets]`, `[migrations]`.
//!
//! Since tonin 0.15 a database or cache is a **reference to the platform
//! contract**, not something tonin renders: the platform layer (CNPG, a Redis
//! manifest, a managed cloud service) provides a Secret with a connection
//! `uri` and pods with agreed labels; the chart reads the Secret into an env
//! var and allows egress to those pods. `[database.<env>]` / `[cache.<env>]`
//! overlays override any field per environment, and a per-env literal `url`
//! replaces the Secret reference for dev setups without contract Secrets.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::plan::{Error, apply_env};

// ---------- on-disk TOML shape ----------

/// Fields of `[database]` / `[database.<env>]` (and the `[databases.<name>]`
/// variants). Everything is optional so the same shape serves the base table
/// and the per-env overlays; `engine` is required on the base table only.
#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawDatabaseFields {
    #[serde(default)]
    pub engine: Option<String>,
    /// Secret holding the connection uri (default `<cluster>-app`).
    #[serde(default)]
    pub secret: Option<String>,
    /// Key of the uri in `secret` (default `uri`).
    #[serde(default)]
    pub key: Option<String>,
    /// Env var the uri lands in (default `DATABASE_URL`).
    #[serde(default)]
    pub env: Option<String>,
    /// `"verify-full"` (mount the CA, verify the server) or `"none"` (default).
    #[serde(default)]
    pub tls: Option<String>,
    /// Secret holding the server CA under key `ca.crt` (default `<cluster>-ca`).
    #[serde(default)]
    pub ca_secret: Option<String>,
    /// CNPG cluster name (default `<service>-pg`): names the default Secrets
    /// and the `cnpg.io/cluster` label egress is allowed to.
    #[serde(default)]
    pub cluster: Option<String>,
    /// Pod labels egress is allowed to; replaces the default selector.
    /// An empty table renders no egress rule.
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    /// Namespace of the database pods (default: the service's namespace).
    #[serde(default)]
    pub namespace: Option<String>,
    /// Egress port (default: the engine's port, 5432 for Postgres).
    #[serde(default)]
    pub port: Option<u32>,
    /// Literal connection URL used instead of the Secret (per env only).
    #[serde(default)]
    pub url: Option<String>,
    // Removed in 0.15 — kept only to reject them with a pointer to the replacement.
    #[serde(default)]
    pub size: Option<toml::Value>,
    #[serde(default)]
    pub shared: Option<toml::Value>,
    #[serde(default)]
    pub version: Option<toml::Value>,
    #[serde(default)]
    pub name: Option<toml::Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawDatabase {
    #[serde(flatten)]
    pub base: RawDatabaseFields,
    #[serde(flatten)]
    pub envs: BTreeMap<String, RawDatabaseFields>,
}

/// Fields of `[cache]` / `[cache.<env>]` (and `[caches.<name>]`).
#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawCacheFields {
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub port: Option<u32>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub size: Option<toml::Value>,
    #[serde(default)]
    pub shared: Option<toml::Value>,
    #[serde(default)]
    pub name: Option<toml::Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawCache {
    #[serde(flatten)]
    pub base: RawCacheFields,
    #[serde(flatten)]
    pub envs: BTreeMap<String, RawCacheFields>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawMigrations {
    pub tool: String,
    #[serde(default = "default_migrations_dir")]
    pub dir: String,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// `"job" | "init-container" | "none"`; `[migrations.<env>] mode` overrides.
    #[serde(default)]
    pub mode: Option<String>,
    /// Removed in 0.15 (replaced by `mode`).
    #[serde(default)]
    pub run_on: Option<toml::Value>,
    #[serde(flatten)]
    pub envs: BTreeMap<String, RawMigrationsEnv>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawMigrationsEnv {
    #[serde(default)]
    pub mode: Option<String>,
}

fn default_migrations_dir() -> String {
    "migrations/".into()
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawSecrets {
    #[serde(default = "default_secret_provider")]
    pub provider: String,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub map: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub external_store: Option<RawExternalStore>,
}

fn default_secret_provider() -> String {
    "k8s".into()
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawExternalStore {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawConfigBlock {
    #[serde(default = "default_config_engine")]
    pub engine: String,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub poll_interval_seconds: Option<u64>,
    #[serde(default)]
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

fn default_config_engine() -> String {
    "env".into()
}

// ---------- normalized types (what the renderer consumes) ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseEngine {
    Postgres,
    Mysql,
    Sqlite,
    Clickhouse,
    None,
}

impl DatabaseEngine {
    pub fn parse(s: &str) -> Self {
        match s {
            "postgres" => Self::Postgres,
            "mysql" => Self::Mysql,
            "sqlite" => Self::Sqlite,
            "clickhouse" => Self::Clickhouse,
            _ => Self::None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Sqlite => "sqlite",
            Self::Clickhouse => "clickhouse",
            Self::None => "none",
        }
    }
    pub fn default_port(&self) -> u32 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql => 3306,
            Self::Clickhouse => 9000,
            Self::Sqlite | Self::None => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheEngine {
    Redis,
    None,
}

impl CacheEngine {
    pub fn parse(s: &str) -> Self {
        match s {
            "redis" => Self::Redis,
            _ => Self::None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Redis => "redis",
            Self::None => "none",
        }
    }
    pub fn default_port(&self) -> u32 {
        match self {
            Self::Redis => 6379,
            Self::None => 0,
        }
    }
}

/// Where a datastore's connection URL comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DatastoreSource {
    /// A key of a platform-contract Secret (the default).
    Secret { name: String, key: String },
    /// A literal URL from a per-env `url` (dev setups without contract Secrets).
    Url(String),
}

/// Pods a datastore's egress rule allows: `labels` in `namespace` (the
/// service's own namespace when `None`) on `port`. No rule when `labels` is
/// empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EgressTarget {
    pub port: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub labels: BTreeMap<String, String>,
}

/// TLS verification for a Postgres connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DatabaseTls {
    /// The URL is used as is.
    #[default]
    None,
    /// Mount the server CA and append `sslmode=verify-full&sslrootcert=…`.
    VerifyFull,
}

/// Fixed directory the Postgres CA (`ca.crt`) of `[database]` is mounted at.
pub const PG_CA_MOUNT_DIR: &str = "/etc/pg-ca";
/// Key of the CA certificate in the CA Secret.
pub const PG_CA_KEY: &str = "ca.crt";

/// A resolved `[database]` (or `[databases.<name>]`) for one environment.
#[derive(Clone, Debug)]
pub struct DatabaseSpec {
    pub engine: DatabaseEngine,
    /// `None` for `[database]`, `Some(name)` for `[databases.<name>]`.
    pub variant: Option<String>,
    /// Env var the connection URL lands in.
    pub env: String,
    pub source: DatastoreSource,
    pub tls: DatabaseTls,
    /// Secret holding `ca.crt` (mounted only when `tls` is verify-full).
    pub ca_secret: String,
    /// `None` for engines without a network endpoint (sqlite).
    pub egress: Option<EgressTarget>,
}

/// A resolved `[cache]` (or `[caches.<name>]`) for one environment.
#[derive(Clone, Debug)]
pub struct CacheSpec {
    pub engine: CacheEngine,
    pub variant: Option<String>,
    pub env: String,
    pub source: DatastoreSource,
    pub egress: Option<EgressTarget>,
}

#[derive(Clone, Debug)]
pub struct SecretsSpec {
    pub provider: SecretProvider,
    pub required: Vec<String>,
    pub map: std::collections::BTreeMap<String, String>,
    pub external_store: Option<ExternalStore>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretProvider {
    K8s,
    ExternalSecrets,
    Vault,
    AwsSecretsManager,
}

impl SecretProvider {
    pub fn parse(s: &str) -> Self {
        match s {
            "external-secrets" => Self::ExternalSecrets,
            "vault" => Self::Vault,
            "aws-secrets-manager" => Self::AwsSecretsManager,
            _ => Self::K8s,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::K8s => "k8s",
            Self::ExternalSecrets => "external-secrets",
            Self::Vault => "vault",
            Self::AwsSecretsManager => "aws-secrets-manager",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExternalStore {
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct ConfigSpec {
    pub engine: ConfigEngine,
    pub path_prefix: Option<String>,
    pub poll_interval_seconds: u64,
    pub endpoints: Vec<String>,
    pub repo: Option<String>,
    pub git_ref: Option<String>,
    pub sources: Vec<ConfigEngine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigEngine {
    Env,
    Etcd,
    Github,
    Chained,
}

impl ConfigEngine {
    pub fn parse(s: &str) -> Self {
        match s {
            "etcd" => Self::Etcd,
            "github" => Self::Github,
            "chained" => Self::Chained,
            _ => Self::Env,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Etcd => "etcd",
            Self::Github => "github",
            Self::Chained => "chained",
        }
    }
}

pub(crate) fn resolve_config(raw: &RawConfigBlock) -> ConfigSpec {
    ConfigSpec {
        engine: ConfigEngine::parse(&raw.engine),
        path_prefix: raw.path_prefix.clone(),
        poll_interval_seconds: raw.poll_interval_seconds.unwrap_or(30),
        endpoints: raw.endpoints.clone(),
        repo: raw.repo.clone(),
        git_ref: raw.git_ref.clone(),
        sources: raw.sources.iter().map(|s| ConfigEngine::parse(s)).collect(),
    }
}

#[derive(Clone, Debug)]
pub struct MigrationsSpec {
    pub tool: MigrationTool,
    pub dir: String,
    /// How migrations run in the selected environment.
    pub mode: MigrationMode,
    pub command: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationTool {
    Sqlx,
    Refinery,
    Flyway,
    Custom,
}

/// How the chart runs migrations (`[migrations] mode`, per env).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationMode {
    /// A pre-install/pre-upgrade hook Job the rollout waits on.
    Job,
    /// An initContainer in every pod of the Deployment.
    #[default]
    InitContainer,
    /// Nothing rendered: the service migrates itself, or someone else does.
    None,
}

impl MigrationMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "job" => Some(Self::Job),
            "init-container" => Some(Self::InitContainer),
            "none" => Some(Self::None),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::InitContainer => "init-container",
            Self::None => "none",
        }
    }
}

// ---------- [callers] with per-env overlay ----------

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub(crate) enum RawCallerEntry {
    Namespace(String),
    Env(std::collections::BTreeMap<String, String>),
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(transparent)]
pub(crate) struct RawCallers(pub std::collections::BTreeMap<String, RawCallerEntry>);

pub(crate) fn resolve_callers(raw: &RawCallers, env: &str) -> Vec<crate::plan::ServiceRef> {
    let mut base = std::collections::BTreeMap::new();
    let mut overlay = std::collections::BTreeMap::new();

    for (key, entry) in &raw.0 {
        match entry {
            RawCallerEntry::Namespace(ns) => {
                // {env} is substituted so `orders-service = "myapp-{env}"` resolves
                // to "myapp-dev", "myapp-prod", etc. per environment.
                base.insert(key.clone(), crate::plan::apply_env(ns, env));
            }
            RawCallerEntry::Env(map) if key == env => {
                overlay = map
                    .iter()
                    .map(|(k, v)| (k.clone(), crate::plan::apply_env(v, env)))
                    .collect();
            }
            RawCallerEntry::Env(_) => {}
        }
    }

    base.extend(overlay);
    base.into_iter()
        .map(|(name, namespace)| crate::plan::ServiceRef { name, namespace })
        .collect()
}

// ---------- env selection ----------

/// Decide which env we're rendering for. Precedence: explicit arg > env var > "dev".
pub fn select_env(explicit: Option<&str>) -> String {
    if let Some(e) = explicit {
        return e.to_string();
    }
    std::env::var("TONIN_ENV").unwrap_or_else(|_| "dev".to_string())
}

/// `database` / `databases.<name>` (or `cache` / `caches.<name>`): the
/// tonin.toml section a datastore comes from, for error messages and values.
fn section_name(kind: &str, variant: Option<&str>) -> String {
    match variant {
        None => kind.to_string(),
        Some(v) => format!("{kind}s.{v}"),
    }
}

/// `[section]` or `[section.<env>]`, for error messages.
fn table_name(section: &str, env: Option<&str>) -> String {
    match env {
        None => format!("[{section}]"),
        Some(e) => format!("[{section}.{e}]"),
    }
}

/// `<service>` for `[database]`/`[cache]`, `<service>-<name>` for a named variant.
fn name_stem(service: &str, variant: Option<&str>) -> String {
    match variant {
        None => service.to_string(),
        Some(v) => format!("{service}-{v}"),
    }
}

/// `WRITE_` for variant `write`; empty for the unnamed section.
fn env_prefix(variant: Option<&str>) -> String {
    variant
        .map(|v| format!("{}_", v.to_uppercase()))
        .unwrap_or_default()
}

fn removed(table: &str, field: &str, hint: &str) -> Error {
    Error::RemovedField {
        field: format!("{table}.{field}"),
        hint: hint.to_string(),
    }
}

const HINT_SHARED: &str = "every database and cache is now a reference to the platform \
     contract (a Secret with key `uri`, e.g. `<service>-pg-app`); remove `shared`. For a dev \
     instance without contract Secrets set a literal `url` under [<section>.<env>]";
const HINT_SIZE: &str = "tonin no longer renders databases or caches; size them in the \
     platform layer (e.g. the CNPG Cluster's storage)";
const HINT_VERSION: &str = "tonin no longer renders databases; set the version in the \
     platform layer (e.g. the CNPG Cluster's image)";
const HINT_NAME: &str = "use `secret` (the Secret holding the uri) and `cluster` or `labels` \
     (the pods the network policy allows egress to)";
const HINT_CACHE_NAME: &str = "use `secret` (the Secret holding the uri) and `labels` \
     (the pods the network policy allows egress to)";

fn reject_removed_db(section: &str, env: Option<&str>, f: &RawDatabaseFields) -> Result<(), Error> {
    let table = table_name(section, env);
    let hint = |h: &str| h.replace("<section>", section);
    if f.shared.is_some() {
        return Err(removed(&table, "shared", &hint(HINT_SHARED)));
    }
    if f.size.is_some() {
        return Err(removed(&table, "size", HINT_SIZE));
    }
    if f.version.is_some() {
        return Err(removed(&table, "version", HINT_VERSION));
    }
    if f.name.is_some() {
        return Err(removed(&table, "name", HINT_NAME));
    }
    if env.is_none() && f.url.is_some() {
        return Err(Error::InvalidField {
            field: format!("{table}.url"),
            reason: format!(
                "`url` is set per environment (e.g. [{section}.dev] url = \"…\" for a dev \
                 instance without contract Secrets); the other environments read the Secret"
            ),
        });
    }
    Ok(())
}

fn reject_removed_cache(section: &str, env: Option<&str>, f: &RawCacheFields) -> Result<(), Error> {
    let table = table_name(section, env);
    if f.shared.is_some() {
        return Err(removed(
            &table,
            "shared",
            &HINT_SHARED.replace("<section>", section),
        ));
    }
    if f.size.is_some() {
        return Err(removed(&table, "size", HINT_SIZE));
    }
    if f.name.is_some() {
        return Err(removed(&table, "name", HINT_CACHE_NAME));
    }
    if env.is_none() && f.url.is_some() {
        return Err(Error::InvalidField {
            field: format!("{table}.url"),
            reason: format!(
                "`url` is set per environment (e.g. [{section}.dev] url = \"…\" for a dev \
                 instance without contract Secrets); the other environments read the Secret"
            ),
        });
    }
    Ok(())
}

/// Field-wise `overlay.or(base)`.
fn merge_db(base: &RawDatabaseFields, overlay: Option<&RawDatabaseFields>) -> RawDatabaseFields {
    let o = overlay.cloned().unwrap_or_default();
    let b = base.clone();
    RawDatabaseFields {
        engine: o.engine.or(b.engine),
        secret: o.secret.or(b.secret),
        key: o.key.or(b.key),
        env: o.env.or(b.env),
        tls: o.tls.or(b.tls),
        ca_secret: o.ca_secret.or(b.ca_secret),
        cluster: o.cluster.or(b.cluster),
        labels: o.labels.or(b.labels),
        namespace: o.namespace.or(b.namespace),
        port: o.port.or(b.port),
        url: o.url,
        size: None,
        shared: None,
        version: None,
        name: None,
    }
}

fn merge_cache(base: &RawCacheFields, overlay: Option<&RawCacheFields>) -> RawCacheFields {
    let o = overlay.cloned().unwrap_or_default();
    let b = base.clone();
    RawCacheFields {
        engine: o.engine.or(b.engine),
        secret: o.secret.or(b.secret),
        key: o.key.or(b.key),
        env: o.env.or(b.env),
        labels: o.labels.or(b.labels),
        namespace: o.namespace.or(b.namespace),
        port: o.port.or(b.port),
        url: o.url,
        size: None,
        shared: None,
        name: None,
    }
}

/// Resolve `[database]` (`variant = None`) or `[databases.<variant>]` for
/// `env`. `None` when the engine is `none`.
pub(crate) fn resolve_database(
    raw: &RawDatabase,
    env: &str,
    service: &str,
    variant: Option<&str>,
) -> Result<Option<DatabaseSpec>, Error> {
    let section = section_name("database", variant);
    reject_removed_db(&section, None, &raw.base)?;
    for (e, overlay) in &raw.envs {
        reject_removed_db(&section, Some(e), overlay)?;
    }

    let f = merge_db(&raw.base, raw.envs.get(env));
    let engine = DatabaseEngine::parse(f.engine.as_deref().ok_or_else(|| Error::InvalidField {
        field: format!("[{section}].engine"),
        reason: "required (e.g. engine = \"postgres\")".into(),
    })?);
    if engine == DatabaseEngine::None {
        return Ok(None);
    }
    let is_pg = engine == DatabaseEngine::Postgres;

    let tls = match f.tls.as_deref() {
        None | Some("none") => DatabaseTls::None,
        Some("verify-full") => DatabaseTls::VerifyFull,
        Some(other) => {
            return Err(Error::InvalidField {
                field: format!("[{section}].tls"),
                reason: format!("{other:?} is not supported; use \"verify-full\" or \"none\""),
            });
        }
    };
    if !is_pg && tls != DatabaseTls::None {
        return Err(Error::InvalidField {
            field: format!("[{section}].tls"),
            reason: format!(
                "only supported for engine = \"postgres\", not {:?}",
                engine.as_str()
            ),
        });
    }
    if !is_pg && f.cluster.is_some() {
        return Err(Error::InvalidField {
            field: format!("[{section}].cluster"),
            reason: format!(
                "names a CNPG cluster and only applies to engine = \"postgres\"; use `secret` \
                 and `labels` for {:?}",
                engine.as_str()
            ),
        });
    }

    let stem = name_stem(service, variant);
    // Contract names. Postgres follows CNPG: Cluster `<stem>-pg` creates
    // Secrets `<cluster>-app` (key `uri`) and `<cluster>-ca` (key `ca.crt`)
    // and labels its pods `cnpg.io/cluster=<cluster>`.
    let (default_secret, default_ca, default_labels) = if is_pg {
        let cluster = f.cluster.clone().unwrap_or_else(|| format!("{stem}-pg"));
        (
            format!("{cluster}-app"),
            format!("{cluster}-ca"),
            BTreeMap::from([("cnpg.io/cluster".to_string(), cluster)]),
        )
    } else {
        (
            format!("{stem}-db"),
            format!("{stem}-db-ca"),
            BTreeMap::from([("app.kubernetes.io/name".to_string(), format!("{stem}-db"))]),
        )
    };

    let source = match f.url {
        Some(url) => DatastoreSource::Url(apply_env(&url, env)),
        None => DatastoreSource::Secret {
            name: f.secret.unwrap_or(default_secret),
            key: f.key.unwrap_or_else(|| "uri".into()),
        },
    };
    let egress = (engine != DatabaseEngine::Sqlite).then(|| EgressTarget {
        port: f.port.unwrap_or_else(|| engine.default_port()),
        namespace: f.namespace.map(|n| apply_env(&n, env)),
        labels: f.labels.unwrap_or(default_labels),
    });

    Ok(Some(DatabaseSpec {
        engine,
        variant: variant.map(str::to_string),
        env: f
            .env
            .unwrap_or_else(|| format!("{}DATABASE_URL", env_prefix(variant))),
        source,
        tls,
        ca_secret: f.ca_secret.unwrap_or(default_ca),
        egress,
    }))
}

/// Resolve `[cache]` (`variant = None`) or `[caches.<variant>]` for `env`.
/// `None` when the engine is `none`.
pub(crate) fn resolve_cache(
    raw: &RawCache,
    env: &str,
    service: &str,
    variant: Option<&str>,
) -> Result<Option<CacheSpec>, Error> {
    let section = section_name("cache", variant);
    reject_removed_cache(&section, None, &raw.base)?;
    for (e, overlay) in &raw.envs {
        reject_removed_cache(&section, Some(e), overlay)?;
    }

    let f = merge_cache(&raw.base, raw.envs.get(env));
    let engine = CacheEngine::parse(f.engine.as_deref().ok_or_else(|| Error::InvalidField {
        field: format!("[{section}].engine"),
        reason: "required (e.g. engine = \"redis\")".into(),
    })?);
    if engine == CacheEngine::None {
        return Ok(None);
    }

    // Contract: Secret `<stem>-cache` (key `uri`), pods labelled
    // `app.kubernetes.io/name=<stem>-cache`.
    let stem = name_stem(service, variant);
    let source = match f.url {
        Some(url) => DatastoreSource::Url(apply_env(&url, env)),
        None => DatastoreSource::Secret {
            name: f.secret.unwrap_or_else(|| format!("{stem}-cache")),
            key: f.key.unwrap_or_else(|| "uri".into()),
        },
    };
    let egress = Some(EgressTarget {
        port: f.port.unwrap_or_else(|| engine.default_port()),
        namespace: f.namespace.map(|n| apply_env(&n, env)),
        labels: f.labels.unwrap_or_else(|| {
            BTreeMap::from([(
                "app.kubernetes.io/name".to_string(),
                format!("{stem}-cache"),
            )])
        }),
    });

    Ok(Some(CacheSpec {
        engine,
        variant: variant.map(str::to_string),
        env: f
            .env
            .unwrap_or_else(|| format!("{}REDIS_URL", env_prefix(variant))),
        source,
        egress,
    }))
}

pub(crate) fn resolve_secrets(raw: &RawSecrets) -> SecretsSpec {
    SecretsSpec {
        provider: SecretProvider::parse(&raw.provider),
        required: raw.required.clone(),
        map: raw.map.clone(),
        external_store: raw.external_store.as_ref().map(|e| ExternalStore {
            name: e.name.clone(),
            kind: e.kind.clone(),
        }),
    }
}

fn parse_mode(table: &str, mode: &str) -> Result<MigrationMode, Error> {
    MigrationMode::parse(mode).ok_or_else(|| Error::InvalidField {
        field: format!("{table}.mode"),
        reason: format!("{mode:?} is not supported; use \"job\", \"init-container\" or \"none\""),
    })
}

pub(crate) fn resolve_migrations(raw: &RawMigrations, env: &str) -> Result<MigrationsSpec, Error> {
    if raw.run_on.is_some() {
        return Err(removed(
            "[migrations]",
            "run_on",
            "use `mode = \"init-container\" | \"job\" | \"none\"` (per environment: \
             [migrations.<env>] mode = …); run_on = \"boot\" or \"manual\" become mode = \"none\"",
        ));
    }
    // Validate every table, not only the selected env, so a typo in another
    // environment fails here instead of at its first deploy.
    let base_mode = match raw.mode.as_deref() {
        Some(m) => parse_mode("[migrations]", m)?,
        None => MigrationMode::default(),
    };
    let mut mode = base_mode;
    for (e, overlay) in &raw.envs {
        if let Some(m) = overlay.mode.as_deref() {
            let parsed = parse_mode(&format!("[migrations.{e}]"), m)?;
            if e == env {
                mode = parsed;
            }
        }
    }

    let tool = match raw.tool.as_str() {
        "refinery" => MigrationTool::Refinery,
        "flyway" => MigrationTool::Flyway,
        "custom" => MigrationTool::Custom,
        _ => MigrationTool::Sqlx,
    };
    let command = match (tool, &raw.command) {
        (MigrationTool::Custom, Some(cmd)) => cmd.clone(),
        (MigrationTool::Sqlx, _) => vec![
            "sqlx".into(),
            "migrate".into(),
            "run".into(),
            "--source".into(),
            raw.dir.clone(),
        ],
        (MigrationTool::Refinery, _) => vec![
            "refinery".into(),
            "migrate".into(),
            "-p".into(),
            raw.dir.clone(),
        ],
        (MigrationTool::Flyway, _) => vec![
            "flyway".into(),
            "-locations=filesystem:".to_string() + &raw.dir,
            "migrate".into(),
        ],
        (MigrationTool::Custom, None) => Vec::new(),
    };
    Ok(MigrationsSpec {
        tool,
        dir: raw.dir.clone(),
        mode,
        command,
    })
}

// ---------- emitted env vars ----------

/// The Secret key a datastore env var is read from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SecretEnvRef {
    /// Env var holding the Secret value: the datastore's own env var, or a
    /// helper (`TONIN_PG_URI`) that `value` composes from with `$(VAR)`.
    pub env: String,
    pub name: String,
    pub key: String,
}

/// A Secret key mounted as a file (the Postgres CA for `verify-full`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaMount {
    /// Pod volume name.
    pub volume: String,
    pub secret: String,
    /// The only key mounted (`ca.crt`).
    pub key: String,
    /// Directory the key is mounted in.
    pub mount_path: String,
}

/// One datastore as the chart's `datastores` values list renders it: the
/// container env var(s), the optional CA volume and the egress rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatastoreEnv {
    /// The tonin.toml section it comes from (`database`, `caches.session`, …).
    pub name: String,
    /// The env var the application reads.
    pub env: String,
    /// Read from this Secret key (into `secret.env`) when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretEnvRef>,
    /// Literal value of `env` (a dev `url`, or the uri composed with TLS
    /// parameters from `$(<secret.env>)`) when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_mount: Option<CaMount>,
    /// `None` when there is nothing to allow (sqlite, or `labels = {}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressTarget>,
}

impl DatastoreEnv {
    /// Every env var this entry declares (helper first), for collision checks.
    pub fn env_names(&self) -> Vec<&str> {
        let mut names = Vec::new();
        if let Some(s) = &self.secret
            && s.env != self.env
        {
            names.push(s.env.as_str());
        }
        names.push(self.env.as_str());
        names
    }
}

/// `pg-ca` / `pg-ca-<variant>`: a DNS-1123 label usable as a volume name.
fn dns_label(prefix: &str, variant: Option<&str>) -> String {
    match variant {
        None => prefix.to_string(),
        Some(v) => format!("{prefix}-{}", v.to_lowercase().replace('_', "-")),
    }
}

#[derive(Clone, Debug, Default)]
pub struct EmittedEnv {
    /// Datastore env vars from `[database]`/`[databases.*]`/`[cache]`/`[caches.*]`.
    pub datastores: Vec<DatastoreEnv>,
    /// Keys read from the service's own `<service>-secrets` Secret (`[secrets].required`).
    pub from_secret: Vec<String>,
}

impl EmittedEnv {
    pub fn extend_database(&mut self, spec: &DatabaseSpec) {
        let variant = spec.variant.as_deref();
        let name = section_name("database", variant);
        let mut entry = DatastoreEnv {
            name,
            env: spec.env.clone(),
            secret: None,
            value: None,
            ca_mount: None,
            egress: spec.egress.clone().filter(|e| !e.labels.is_empty()),
        };

        let ca_dir = match variant {
            None => PG_CA_MOUNT_DIR.to_string(),
            Some(_) => format!("/etc/{}", dns_label("pg-ca", variant)),
        };
        let tls_params = format!("sslmode=verify-full&sslrootcert={ca_dir}/{PG_CA_KEY}");
        let verify = spec.tls == DatabaseTls::VerifyFull;

        match &spec.source {
            DatastoreSource::Secret { name, key } => {
                // With verify-full the raw uri goes into a helper var and the
                // app's var appends the TLS parameters through Kubernetes
                // `$(VAR)` expansion (the helper is declared first).
                let secret_env = if verify {
                    format!("TONIN_{}PG_URI", env_prefix(variant))
                } else {
                    spec.env.clone()
                };
                if verify {
                    entry.value = Some(format!("$({secret_env})?{tls_params}"));
                }
                entry.secret = Some(SecretEnvRef {
                    env: secret_env,
                    name: name.clone(),
                    key: key.clone(),
                });
            }
            DatastoreSource::Url(url) => {
                entry.value = Some(if verify {
                    let sep = if url.contains('?') { '&' } else { '?' };
                    format!("{url}{sep}{tls_params}")
                } else {
                    url.clone()
                });
            }
        }
        if verify {
            entry.ca_mount = Some(CaMount {
                volume: dns_label("pg-ca", variant),
                secret: spec.ca_secret.clone(),
                key: PG_CA_KEY.into(),
                mount_path: ca_dir,
            });
        }
        self.datastores.push(entry);
    }

    pub fn extend_cache(&mut self, spec: &CacheSpec) {
        let (secret, value) = match &spec.source {
            DatastoreSource::Secret { name, key } => (
                Some(SecretEnvRef {
                    env: spec.env.clone(),
                    name: name.clone(),
                    key: key.clone(),
                }),
                None,
            ),
            DatastoreSource::Url(url) => (None, Some(url.clone())),
        };
        self.datastores.push(DatastoreEnv {
            name: section_name("cache", spec.variant.as_deref()),
            env: spec.env.clone(),
            secret,
            value,
            ca_mount: None,
            egress: spec.egress.clone().filter(|e| !e.labels.is_empty()),
        });
    }

    pub fn extend_secrets(&mut self, spec: &SecretsSpec) {
        for key in &spec.required {
            self.from_secret.push(key.clone());
        }
    }

    /// Every env var tonin emits, with the tonin.toml section it comes from,
    /// in render order.
    pub fn declared(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .from_secret
            .iter()
            .map(|k| (k.clone(), "[secrets].required".to_string()))
            .collect();
        for d in &self.datastores {
            for n in d.env_names() {
                out.push((n.to_string(), format!("[{}]", d.name)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section<T: serde::de::DeserializeOwned>(toml_str: &str, key: &str) -> T {
        toml::from_str::<toml::Value>(toml_str)
            .unwrap()
            .get(key)
            .unwrap()
            .clone()
            .try_into()
            .unwrap()
    }

    fn db(toml_str: &str, env: &str) -> Result<Option<DatabaseSpec>, Error> {
        let raw: RawDatabase = section(toml_str, "database");
        resolve_database(&raw, env, "billing", None)
    }

    fn cache(toml_str: &str, env: &str) -> Result<Option<CacheSpec>, Error> {
        let raw: RawCache = section(toml_str, "cache");
        resolve_cache(&raw, env, "billing", None)
    }

    fn emitted_db(spec: &DatabaseSpec) -> DatastoreEnv {
        let mut env = EmittedEnv::default();
        env.extend_database(spec);
        env.datastores.remove(0)
    }

    #[test]
    fn postgres_defaults_follow_the_platform_contract() {
        let spec = db("[database]\nengine = \"postgres\"", "prd")
            .unwrap()
            .unwrap();
        assert_eq!(spec.env, "DATABASE_URL");
        assert_eq!(
            spec.source,
            DatastoreSource::Secret {
                name: "billing-pg-app".into(),
                key: "uri".into()
            }
        );
        assert_eq!(spec.ca_secret, "billing-pg-ca");
        let egress = spec.egress.clone().unwrap();
        assert_eq!(egress.port, 5432);
        assert_eq!(egress.namespace, None);
        assert_eq!(
            egress.labels,
            BTreeMap::from([("cnpg.io/cluster".into(), "billing-pg".into())])
        );

        let d = emitted_db(&spec);
        assert_eq!(
            d.secret,
            Some(SecretEnvRef {
                env: "DATABASE_URL".into(),
                name: "billing-pg-app".into(),
                key: "uri".into()
            })
        );
        assert_eq!(d.value, None);
        assert_eq!(d.ca_mount, None);
    }

    #[test]
    fn database_names_are_overridable_per_service_and_env() {
        let toml = r#"
            [database]
            engine = "postgres"
            env    = "PLATFORM_DATABASE_URL"
            cluster = "api-pg"

            [database.prd]
            secret = "api-db-creds"
            key    = "url"
            port   = 6432
        "#;
        let dev = db(toml, "dev").unwrap().unwrap();
        assert_eq!(dev.env, "PLATFORM_DATABASE_URL");
        assert_eq!(
            dev.source,
            DatastoreSource::Secret {
                name: "api-pg-app".into(),
                key: "uri".into()
            }
        );
        assert_eq!(dev.ca_secret, "api-pg-ca");
        assert_eq!(
            dev.egress.unwrap().labels,
            BTreeMap::from([("cnpg.io/cluster".into(), "api-pg".into())])
        );

        let prd = db(toml, "prd").unwrap().unwrap();
        assert_eq!(
            prd.source,
            DatastoreSource::Secret {
                name: "api-db-creds".into(),
                key: "url".into()
            }
        );
        assert_eq!(prd.egress.unwrap().port, 6432);
    }

    #[test]
    fn verify_full_composes_the_url_from_a_helper_var_and_mounts_the_ca() {
        let toml = r#"
            [database]
            engine = "postgres"
            tls    = "verify-full"
        "#;
        let d = emitted_db(&db(toml, "prd").unwrap().unwrap());
        assert_eq!(
            d.secret,
            Some(SecretEnvRef {
                env: "TONIN_PG_URI".into(),
                name: "billing-pg-app".into(),
                key: "uri".into()
            })
        );
        assert_eq!(
            d.value.as_deref(),
            Some("$(TONIN_PG_URI)?sslmode=verify-full&sslrootcert=/etc/pg-ca/ca.crt")
        );
        assert_eq!(
            d.ca_mount,
            Some(CaMount {
                volume: "pg-ca".into(),
                secret: "billing-pg-ca".into(),
                key: "ca.crt".into(),
                mount_path: "/etc/pg-ca".into(),
            })
        );
        assert_eq!(d.env_names(), vec!["TONIN_PG_URI", "DATABASE_URL"]);
    }

    #[test]
    fn verify_full_on_a_literal_url_appends_with_the_right_separator() {
        let toml = r#"
            [database]
            engine = "postgres"
            tls    = "verify-full"

            [database.dev]
            url = "postgresql://u:p@pg.shared-dev:5432/app?application_name=billing"

            [database.stg]
            url = "postgresql://u:p@pg.stg:5432/app"
        "#;
        let dev = emitted_db(&db(toml, "dev").unwrap().unwrap());
        assert_eq!(dev.secret, None);
        assert_eq!(
            dev.value.as_deref(),
            Some(
                "postgresql://u:p@pg.shared-dev:5432/app?application_name=billing\
                 &sslmode=verify-full&sslrootcert=/etc/pg-ca/ca.crt"
            )
        );
        let stg = emitted_db(&db(toml, "stg").unwrap().unwrap());
        assert!(
            stg.value
                .as_deref()
                .unwrap()
                .ends_with("/app?sslmode=verify-full&sslrootcert=/etc/pg-ca/ca.crt")
        );
    }

    #[test]
    fn dev_literal_url_replaces_the_secret_reference() {
        let toml = r#"
            [database]
            engine = "postgres"

            [database.dev]
            url       = "postgresql://postgres:postgres@postgres.shared-{env}:5432/billing"
            namespace = "shared-{env}"
            labels    = { app = "postgres" }
        "#;
        let dev = db(toml, "dev").unwrap().unwrap();
        assert_eq!(
            dev.source,
            DatastoreSource::Url(
                "postgresql://postgres:postgres@postgres.shared-dev:5432/billing".into()
            )
        );
        let egress = dev.egress.unwrap();
        assert_eq!(egress.namespace.as_deref(), Some("shared-dev"));
        assert_eq!(
            egress.labels,
            BTreeMap::from([("app".into(), "postgres".into())])
        );
        // Other envs keep the contract Secret.
        let prd = db(toml, "prd").unwrap().unwrap();
        assert!(matches!(prd.source, DatastoreSource::Secret { .. }));
        assert_eq!(prd.egress.unwrap().namespace, None);
    }

    #[test]
    fn empty_labels_render_no_egress_rule() {
        let toml = "[database]\nengine = \"postgres\"\nlabels = {}";
        let d = emitted_db(&db(toml, "prd").unwrap().unwrap());
        assert_eq!(d.egress, None);
    }

    #[test]
    fn removed_database_fields_fail_with_their_replacement() {
        for (toml, field, hint) in [
            (
                "[database]\nengine = \"postgres\"\nsize = \"10Gi\"",
                "[database].size",
                "platform layer",
            ),
            (
                "[database]\nengine = \"postgres\"\n[database.dev]\nshared = true",
                "[database.dev].shared",
                "[database.<env>]",
            ),
            (
                "[database]\nengine = \"postgres\"\nversion = \"16\"",
                "[database].version",
                "platform layer",
            ),
            (
                "[database]\nengine = \"postgres\"\n[database.dev]\nname = \"postgres\"",
                "[database.dev].name",
                "`secret`",
            ),
        ] {
            let err = db(toml, "prd").unwrap_err().to_string();
            assert!(err.contains(field), "{err}");
            assert!(err.contains(hint), "{err}");
        }
        let err = db(
            "[database]\nengine = \"postgres\"\nurl = \"postgresql://x\"",
            "dev",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("[database].url"), "{err}");
        assert!(err.contains("[database.dev]"), "{err}");
    }

    #[test]
    fn invalid_tls_and_cluster_are_rejected() {
        let err = db(
            "[database]\nengine = \"postgres\"\ntls = \"require\"",
            "prd",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("verify-full"), "{err}");
        let err = db(
            "[database]\nengine = \"mysql\"\ntls = \"verify-full\"",
            "prd",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("postgres"), "{err}");
        let err = db("[database]\nengine = \"mysql\"\ncluster = \"x\"", "prd")
            .unwrap_err()
            .to_string();
        assert!(err.contains("[database].cluster"), "{err}");
    }

    #[test]
    fn engine_none_emits_nothing() {
        assert!(
            db("[database]\nengine = \"none\"", "prd")
                .unwrap()
                .is_none()
        );
        assert!(
            db(
                "[database]\nengine = \"postgres\"\n[database.dev]\nengine = \"none\"",
                "dev"
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn named_variants_get_prefixed_names() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            databases: BTreeMap<String, RawDatabase>,
            caches: BTreeMap<String, RawCache>,
        }
        let w: Wrapper = toml::from_str(
            r#"
            [databases.write]
            engine = "postgres"
            tls    = "verify-full"

            [caches.session]
            engine = "redis"
        "#,
        )
        .unwrap();
        let d = resolve_database(&w.databases["write"], "prd", "app", Some("write"))
            .unwrap()
            .unwrap();
        let c = resolve_cache(&w.caches["session"], "prd", "app", Some("session"))
            .unwrap()
            .unwrap();
        let mut env = EmittedEnv::default();
        env.extend_database(&d);
        env.extend_cache(&c);
        let (db_env, cache_env) = (&env.datastores[0], &env.datastores[1]);

        assert_eq!(db_env.name, "databases.write");
        assert_eq!(db_env.env, "WRITE_DATABASE_URL");
        let secret = db_env.secret.as_ref().unwrap();
        assert_eq!(secret.env, "TONIN_WRITE_PG_URI");
        assert_eq!(secret.name, "app-write-pg-app");
        let ca = db_env.ca_mount.as_ref().unwrap();
        assert_eq!(ca.volume, "pg-ca-write");
        assert_eq!(ca.secret, "app-write-pg-ca");
        assert_eq!(ca.mount_path, "/etc/pg-ca-write");
        assert_eq!(
            db_env.value.as_deref(),
            Some("$(TONIN_WRITE_PG_URI)?sslmode=verify-full&sslrootcert=/etc/pg-ca-write/ca.crt")
        );

        assert_eq!(cache_env.name, "caches.session");
        assert_eq!(cache_env.env, "SESSION_REDIS_URL");
        assert_eq!(cache_env.secret.as_ref().unwrap().name, "app-session-cache");
        assert_eq!(
            cache_env.egress.as_ref().unwrap().labels,
            BTreeMap::from([("app.kubernetes.io/name".into(), "app-session-cache".into())])
        );
    }

    #[test]
    fn cache_defaults_overrides_and_dev_url() {
        let toml = r#"
            [cache]
            engine = "redis"
            env    = "PLATFORM_REDIS_URL"

            [cache.dev]
            url = "redis://redis.shared-dev.svc.cluster.local:6379"
        "#;
        let prd = cache(toml, "prd").unwrap().unwrap();
        assert_eq!(prd.env, "PLATFORM_REDIS_URL");
        assert_eq!(
            prd.source,
            DatastoreSource::Secret {
                name: "billing-cache".into(),
                key: "uri".into()
            }
        );
        let egress = prd.egress.unwrap();
        assert_eq!(egress.port, 6379);
        assert_eq!(
            egress.labels,
            BTreeMap::from([("app.kubernetes.io/name".into(), "billing-cache".into())])
        );
        let dev = cache(toml, "dev").unwrap().unwrap();
        let mut env = EmittedEnv::default();
        env.extend_cache(&dev);
        assert_eq!(
            env.datastores[0].value.as_deref(),
            Some("redis://redis.shared-dev.svc.cluster.local:6379")
        );
        assert_eq!(env.datastores[0].secret, None);
    }

    #[test]
    fn removed_cache_fields_fail() {
        let err = cache("[cache]\nengine = \"redis\"\nshared = false", "prd")
            .unwrap_err()
            .to_string();
        assert!(err.contains("[cache].shared"), "{err}");
        assert!(err.contains("[cache.<env>]"), "{err}");
        let err = cache("[cache]\nengine = \"redis\"\nsize = \"1Gi\"", "prd")
            .unwrap_err()
            .to_string();
        assert!(err.contains("[cache].size"), "{err}");
    }

    fn migrations(toml_str: &str, env: &str) -> Result<MigrationsSpec, Error> {
        let raw: RawMigrations = section(toml_str, "migrations");
        resolve_migrations(&raw, env)
    }

    #[test]
    fn migrations_sqlx_command_default() {
        let spec = migrations("[migrations]\ntool = \"sqlx\"", "prd").unwrap();
        assert_eq!(spec.tool, MigrationTool::Sqlx);
        assert_eq!(spec.mode, MigrationMode::InitContainer);
        assert_eq!(
            spec.command,
            vec!["sqlx", "migrate", "run", "--source", "migrations/"]
        );
    }

    #[test]
    fn migrations_custom_requires_command() {
        let spec = migrations(
            "[migrations]\ntool = \"custom\"\ncommand = [\"./migrate.sh\", \"--all\"]",
            "prd",
        )
        .unwrap();
        assert_eq!(spec.tool, MigrationTool::Custom);
        assert_eq!(spec.command, vec!["./migrate.sh", "--all"]);
    }

    #[test]
    fn migrations_mode_is_set_per_env() {
        let toml = r#"
            [migrations]
            tool = "sqlx"
            mode = "init-container"

            [migrations.prd]
            mode = "job"

            [migrations.test]
            mode = "none"
        "#;
        assert_eq!(
            migrations(toml, "dev").unwrap().mode,
            MigrationMode::InitContainer
        );
        assert_eq!(migrations(toml, "prd").unwrap().mode, MigrationMode::Job);
        assert_eq!(migrations(toml, "test").unwrap().mode, MigrationMode::None);
    }

    #[test]
    fn migrations_reject_run_on_and_unknown_modes() {
        let err = migrations("[migrations]\ntool = \"sqlx\"\nrun_on = \"boot\"", "prd")
            .unwrap_err()
            .to_string();
        assert!(err.contains("[migrations].run_on"), "{err}");
        assert!(err.contains("mode = \"none\""), "{err}");
        // A typo in another env's table fails too.
        let err = migrations(
            "[migrations]\ntool = \"sqlx\"\n[migrations.prd]\nmode = \"jobs\"",
            "dev",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("[migrations.prd].mode"), "{err}");
    }

    #[test]
    fn secrets_default_provider_is_k8s() {
        let raw = RawSecrets {
            provider: default_secret_provider(),
            required: vec!["JWT_SIGNING_KEY".into()],
            map: Default::default(),
            external_store: None,
        };
        let spec = resolve_secrets(&raw);
        assert_eq!(spec.provider, SecretProvider::K8s);
        assert_eq!(spec.required, vec!["JWT_SIGNING_KEY".to_string()]);
    }

    fn parse_callers(toml_str: &str) -> RawCallers {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            callers: RawCallers,
        }
        toml::from_str::<Wrapper>(toml_str).unwrap().callers
    }

    #[test]
    fn callers_base_only_no_overlay() {
        let raw = parse_callers(
            r#"
            [callers]
            orders-service    = "myapp"
            inventory-service = "myapp"
        "#,
        );
        let callers = resolve_callers(&raw, "dev");
        assert_eq!(callers.len(), 2);
        assert!(callers.iter().all(|c| c.namespace == "myapp"));
    }

    #[test]
    fn callers_dev_overlay_overrides_namespace() {
        let raw = parse_callers(
            r#"
            [callers]
            orders-service    = "myapp"
            inventory-service = "myapp"

            [callers.dev]
            orders-service    = "myapp-dev"
            inventory-service = "myapp-dev"
        "#,
        );
        let dev = resolve_callers(&raw, "dev");
        assert!(
            dev.iter().all(|c| c.namespace == "myapp-dev"),
            "dev overlay must win"
        );
        let prod = resolve_callers(&raw, "prod");
        assert!(
            prod.iter().all(|c| c.namespace == "myapp"),
            "prod falls back to base"
        );
    }

    #[test]
    fn callers_dev_overlay_adds_new_caller() {
        let raw = parse_callers(
            r#"
            [callers]
            orders-service = "myapp"

            [callers.dev]
            orders-service = "myapp-dev"
            debug-tool     = "myapp-dev"
        "#,
        );
        let dev = resolve_callers(&raw, "dev");
        assert_eq!(dev.len(), 2, "overlay adds debug-tool");
        let prod = resolve_callers(&raw, "prod");
        assert_eq!(prod.len(), 1, "prod sees base only");
    }

    #[test]
    fn callers_env_placeholder_resolves_per_env() {
        let raw = parse_callers(
            r#"
            [callers]
            orders-service    = "myapp-{env}"
            inventory-service = "myapp-{env}"
        "#,
        );
        let dev = resolve_callers(&raw, "dev");
        assert!(
            dev.iter().all(|c| c.namespace == "myapp-dev"),
            "dev: {{env}} -> -dev"
        );
        let staging = resolve_callers(&raw, "staging");
        assert!(
            staging.iter().all(|c| c.namespace == "myapp-staging"),
            "staging: {{env}} -> -staging"
        );
        let prod = resolve_callers(&raw, "prod");
        assert!(
            prod.iter().all(|c| c.namespace == "myapp-prod"),
            "prod: {{env}} -> -prod"
        );
    }

    #[test]
    fn callers_env_placeholder_with_prod_override() {
        // {env} in base; explicit [callers.prod] overrides just prod.
        let raw = parse_callers(
            r#"
            [callers]
            orders-service    = "myapp-{env}"
            inventory-service = "myapp-{env}"

            [callers.prod]
            orders-service    = "myapp"
            inventory-service = "myapp"
        "#,
        );
        let dev = resolve_callers(&raw, "dev");
        assert!(dev.iter().all(|c| c.namespace == "myapp-dev"));
        let staging = resolve_callers(&raw, "staging");
        assert!(staging.iter().all(|c| c.namespace == "myapp-staging"));
        let prod = resolve_callers(&raw, "prod");
        assert!(
            prod.iter().all(|c| c.namespace == "myapp"),
            "prod override wins over {{env}}"
        );
    }

    #[test]
    fn env_selection_precedence() {
        unsafe { std::env::set_var("TONIN_ENV", "staging") };
        assert_eq!(select_env(Some("prod")), "prod");
        assert_eq!(select_env(None), "staging");
        unsafe { std::env::remove_var("TONIN_ENV") };
        assert_eq!(select_env(None), "dev");
    }
}
