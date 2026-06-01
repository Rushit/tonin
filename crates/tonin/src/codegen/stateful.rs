//! Stateful dependencies (Phase 1 of stateful-deps design).
//!
//! Loads `[database]`, `[cache]`, `[secrets]`, `[migrations]` from
//! `tonin.toml`, applies `[database.dev]` / `[database.prod]` env-overlay,
//! and normalizes into types the renderer will consume.
//!
//! What's NOT here yet (Phase 2+):
//! - k8s template rendering (StatefulSet / PVC / Secret / initContainer)
//! - runtime traits (Database / Cache / EventBus / SecretStore)
//! - any redis/postgres client wiring
//!
//! Open Question recommendations baked in (per docs/design/stateful-deps.md):
//! - Q1 = one DB per service, strictly.
//! - Q3 = secret provider per-service.
//! - Q4 = framework emits `DATABASE_URL` env var; service reads it.
//! - Q5 = one migration set per service (single command).

use serde::{Deserialize, Serialize};

// ---------- on-disk TOML shape ----------
//
// `[database]` / `[cache]` / `[secrets]` / `[migrations]` are all optional.
// A service that doesn't need a capability simply omits the section.

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawDatabase {
    /// "postgres" | "mysql" | "sqlite" | "clickhouse" | "none"
    pub engine: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    /// `false` (default) = framework provisions a per-service StatefulSet.
    /// `true` = service points env vars at an existing cluster instance.
    #[serde(default)]
    pub shared: bool,
    /// Optional override; defaults to `<service-name>-db` in owned mode,
    /// or the shared service name in shared mode.
    #[serde(default)]
    pub name: Option<String>,
    /// Only meaningful when `shared = true`. Defaults to the service's
    /// namespace.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Env-overlay subtables. `dev`/`prod`/etc. Match by `--env` /
    /// `TONIN_ENV` / default `"dev"`.
    #[serde(default, flatten)]
    pub envs: std::collections::BTreeMap<String, RawDatabaseEnv>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawDatabaseEnv {
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub shared: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawCache {
    pub engine: String, // "redis" | "none"
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub shared: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default, flatten)]
    pub envs: std::collections::BTreeMap<String, RawCacheEnv>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawCacheEnv {
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub shared: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawSecrets {
    /// "k8s" (default) | "external-secrets" | "vault" | "aws-secrets-manager"
    #[serde(default = "default_secret_provider")]
    pub provider: String,
    /// Secret KEYS the service requires at runtime. Values come from
    /// outside the framework (user fills in placeholder Secret, or
    /// ExternalSecret resource sources them).
    #[serde(default)]
    pub required: Vec<String>,
    /// Optional env-var → secret-key remap. Used when the env var name
    /// the service code reads differs from the secret key name.
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
    pub kind: String, // "ClusterSecretStore" | "SecretStore"
}

/// Dynamic application config block. Independent of `[secrets]`, which is
/// for credentials. The `engine` selects which `Config` impl (env / etcd /
/// github / chained) the service uses at runtime; impl crates live in
/// `tonin-config-<engine>`.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawConfigBlock {
    /// "env" (default) | "etcd" | "github" | "chained"
    #[serde(default = "default_config_engine")]
    pub engine: String,
    /// Optional prefix prepended to lookup paths by the runtime
    /// (e.g. "/myservice/" for etcd, "services/myservice/" for github).
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Polling cadence for engines that can't push (github). Default 30s.
    #[serde(default)]
    pub poll_interval_seconds: Option<u64>,
    /// engine = "etcd" only.
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// engine = "github" only.
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    /// engine = "chained" only — ordered list of sub-engines to chain.
    #[serde(default)]
    pub sources: Vec<String>,
}

fn default_config_engine() -> String {
    "env".into()
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawMigrations {
    /// "sqlx" | "refinery" | "flyway" | "custom"
    pub tool: String,
    #[serde(default = "default_migrations_dir")]
    pub dir: String,
    /// "init-container" (default; safest) | "boot" | "manual"
    #[serde(default = "default_run_on")]
    pub run_on: String,
    /// Required when tool = "custom"; ignored otherwise.
    #[serde(default)]
    pub command: Option<Vec<String>>,
}

fn default_migrations_dir() -> String {
    "migrations/".into()
}

fn default_run_on() -> String {
    "init-container".into()
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
    /// Default TCP port; used when composing the URL.
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

/// Resolved database spec for the selected environment.
#[derive(Clone, Debug)]
pub struct DatabaseSpec {
    pub engine: DatabaseEngine,
    pub version: String,
    pub size: String,
    pub shared: bool,
    /// Where the service will reach the DB.
    /// - owned mode: `<service>-db.<service-namespace>.svc.cluster.local`
    /// - shared mode: `<name>.<namespace>.svc.cluster.local` (both required)
    pub name: String,
    pub namespace: String,
}

/// Default version per engine (Phase 2 sub-decision B).
fn default_db_version(engine: DatabaseEngine) -> String {
    match engine {
        DatabaseEngine::Postgres => "18".into(),
        DatabaseEngine::Mysql => "8".into(),
        DatabaseEngine::Clickhouse => "24.3".into(),
        DatabaseEngine::Sqlite | DatabaseEngine::None => "latest".into(),
    }
}

impl DatabaseSpec {
    /// Docker image reference for this engine + version. Phase 2 default
    /// for Postgres is `postgres:18` (Docker Hub official).
    pub fn image(&self) -> String {
        match self.engine {
            DatabaseEngine::Postgres => format!("postgres:{}", self.version),
            DatabaseEngine::Mysql => format!("mysql:{}", self.version),
            DatabaseEngine::Clickhouse => format!("clickhouse/clickhouse-server:{}", self.version),
            DatabaseEngine::Sqlite | DatabaseEngine::None => "".into(),
        }
    }
    /// Hostname the runtime client will connect to.
    pub fn host(&self) -> String {
        format!("{}.{}.svc.cluster.local", self.name, self.namespace)
    }
    pub fn port(&self) -> u32 {
        self.engine.default_port()
    }
    /// `DATABASE_URL` value the service container receives at runtime.
    /// Password substituted in by env var expansion at deploy time:
    /// e.g. `postgres://service:$DATABASE_PASSWORD@host:5432/dbname`.
    /// The framework does NOT compose the URL itself (per Q4); this
    /// helper is for the renderer to emit it as a literal env var.
    pub fn url_template(&self, service_name: &str) -> String {
        format!(
            "{}://{svc}:$DATABASE_PASSWORD@{host}:{port}/{svc}",
            self.engine.as_str(),
            svc = service_name,
            host = self.host(),
            port = self.port(),
        )
    }
}

/// Resolved cache spec for the selected environment.
#[derive(Clone, Debug)]
pub struct CacheSpec {
    pub engine: CacheEngine,
    pub size: String,
    pub shared: bool,
    pub name: String,
    pub namespace: String,
}

impl CacheSpec {
    pub fn host(&self) -> String {
        format!("{}.{}.svc.cluster.local", self.name, self.namespace)
    }
    pub fn port(&self) -> u32 {
        self.engine.default_port()
    }
    /// `REDIS_URL` value.
    pub fn url(&self) -> String {
        format!("redis://{}:{}", self.host(), self.port())
    }
}

/// Resolved secrets spec.
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

/// Resolved dynamic-config spec.
#[derive(Clone, Debug)]
pub struct ConfigSpec {
    pub engine: ConfigEngine,
    pub path_prefix: Option<String>,
    pub poll_interval_seconds: u64,
    /// engine = "etcd": one or more etcd endpoints.
    pub endpoints: Vec<String>,
    /// engine = "github": "owner/repo" + ref + path prefix.
    pub repo: Option<String>,
    pub git_ref: Option<String>,
    /// engine = "chained": sub-engines, evaluated first-hit-wins.
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

/// Resolved migrations spec.
#[derive(Clone, Debug)]
pub struct MigrationsSpec {
    pub tool: MigrationTool,
    pub dir: String,
    pub run_on: MigrationRunOn,
    /// Command for `tool = custom`. Computed for known tools.
    pub command: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationTool {
    Sqlx,
    Refinery,
    Flyway,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationRunOn {
    InitContainer,
    Boot,
    Manual,
}

// ---------- env selection + overlay resolution ----------

/// Decide which env we're rendering for. Precedence: explicit arg > env var
/// > default "dev".
pub fn select_env(explicit: Option<&str>) -> String {
    if let Some(e) = explicit {
        return e.to_string();
    }
    std::env::var("TONIN_ENV").unwrap_or_else(|_| "dev".to_string())
}

pub(crate) fn resolve_database(
    raw: &RawDatabase,
    env: &str,
    service_name: &str,
    service_namespace: &str,
) -> DatabaseSpec {
    let overlay = raw.envs.get(env);
    let shared = overlay.and_then(|o| o.shared).unwrap_or(raw.shared);
    let engine = DatabaseEngine::parse(
        overlay
            .and_then(|o| o.engine.as_deref())
            .unwrap_or(&raw.engine),
    );
    let version = overlay
        .and_then(|o| o.version.clone())
        .or_else(|| raw.version.clone())
        .unwrap_or_else(|| default_db_version(engine));
    let size = overlay
        .and_then(|o| o.size.clone())
        .or_else(|| raw.size.clone())
        .unwrap_or_else(|| "2Gi".into());
    // Default name: `<service>-db` in owned mode; the cluster's actual
    // shared service in shared mode (no default — must be provided).
    let name = overlay
        .and_then(|o| o.name.clone())
        .or_else(|| raw.name.clone())
        .unwrap_or_else(|| format!("{}-db", service_name));
    let namespace = overlay
        .and_then(|o| o.namespace.clone())
        .or_else(|| raw.namespace.clone())
        .unwrap_or_else(|| service_namespace.to_string());

    DatabaseSpec {
        engine,
        version,
        size,
        shared,
        name,
        namespace,
    }
}

pub(crate) fn resolve_cache(
    raw: &RawCache,
    env: &str,
    service_name: &str,
    service_namespace: &str,
) -> CacheSpec {
    let overlay = raw.envs.get(env);
    let shared = overlay.and_then(|o| o.shared).unwrap_or(raw.shared);
    let engine = CacheEngine::parse(
        overlay
            .and_then(|o| o.engine.as_deref())
            .unwrap_or(&raw.engine),
    );
    let size = overlay
        .and_then(|o| o.size.clone())
        .or_else(|| raw.size.clone())
        .unwrap_or_else(|| "1Gi".into());
    let name = overlay
        .and_then(|o| o.name.clone())
        .or_else(|| raw.name.clone())
        .unwrap_or_else(|| format!("{}-cache", service_name));
    let namespace = overlay
        .and_then(|o| o.namespace.clone())
        .or_else(|| raw.namespace.clone())
        .unwrap_or_else(|| service_namespace.to_string());

    CacheSpec {
        engine,
        size,
        shared,
        name,
        namespace,
    }
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

pub(crate) fn resolve_migrations(raw: &RawMigrations) -> MigrationsSpec {
    let tool = match raw.tool.as_str() {
        "refinery" => MigrationTool::Refinery,
        "flyway" => MigrationTool::Flyway,
        "custom" => MigrationTool::Custom,
        _ => MigrationTool::Sqlx,
    };
    let run_on = match raw.run_on.as_str() {
        "boot" => MigrationRunOn::Boot,
        "manual" => MigrationRunOn::Manual,
        _ => MigrationRunOn::InitContainer,
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
        (MigrationTool::Refinery, _) => {
            vec![
                "refinery".into(),
                "migrate".into(),
                "-p".into(),
                raw.dir.clone(),
            ]
        }
        (MigrationTool::Flyway, _) => {
            vec![
                "flyway".into(),
                "-locations=filesystem:".to_string() + &raw.dir,
                "migrate".into(),
            ]
        }
        (MigrationTool::Custom, None) => Vec::new(),
    };
    MigrationsSpec {
        tool,
        dir: raw.dir.clone(),
        run_on,
        command,
    }
}

// ---------- emitted env vars (what the deployment template will use) ----------

/// Env vars the deployment.yaml must set on the service container so the
/// runtime can connect to the resolved DB/cache/secret store.
///
/// Phase 2 (renderer) will consume this; Phase 1 just produces it.
#[derive(Clone, Debug, Default)]
pub struct EmittedEnv {
    /// Plaintext literal values (e.g., URLs). Safe to put in deployment.yaml.
    pub literals: Vec<(String, String)>,
    /// Names of env vars that should be sourced from a Secret via
    /// `valueFrom: secretKeyRef`.
    pub from_secret: Vec<String>,
}

impl EmittedEnv {
    pub fn extend_database(&mut self, spec: &DatabaseSpec, service_name: &str) {
        if matches!(spec.engine, DatabaseEngine::None) {
            return;
        }
        self.literals
            .push(("DATABASE_URL".into(), spec.url_template(service_name)));
        // Password ALWAYS from secret; never literal.
        self.from_secret.push("DATABASE_PASSWORD".into());
    }
    pub fn extend_cache(&mut self, spec: &CacheSpec) {
        if matches!(spec.engine, CacheEngine::None) {
            return;
        }
        self.literals.push(("REDIS_URL".into(), spec.url()));
    }
    pub fn extend_secrets(&mut self, spec: &SecretsSpec) {
        for key in &spec.required {
            // Honor env→secret-key remap if present; emit the env var name
            // the service code reads. The actual key inside the Secret is
            // determined when the Secret manifest is rendered (Phase 2).
            self.from_secret.push(key.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toml_to_raw_db(s: &str) -> RawDatabase {
        toml::from_str::<toml::Value>(s)
            .unwrap()
            .get("database")
            .unwrap()
            .clone()
            .try_into()
            .unwrap()
    }

    #[test]
    fn db_overlay_dev_wins_over_top_level() {
        let toml = r#"
            [database]
            engine = "postgres"
            shared = false
            size = "10Gi"

            [database.dev]
            shared = true
            name = "postgres"
            namespace = "shared-dev"
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "dev", "billing", "billing-ns");
        assert!(spec.shared, "dev overlay forces shared=true");
        assert_eq!(spec.name, "postgres");
        assert_eq!(spec.namespace, "shared-dev");
        // Engine, size from top-level (not overridden).
        assert_eq!(spec.engine, DatabaseEngine::Postgres);
        assert_eq!(spec.size, "10Gi");
    }

    #[test]
    fn db_prod_uses_owned_defaults() {
        let toml = r#"
            [database]
            engine = "postgres"
            shared = false
            size = "10Gi"

            [database.dev]
            shared = true
            name = "postgres"
            namespace = "shared-dev"
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "prod", "billing", "billing-ns");
        // No [database.prod] overlay → top-level applies.
        assert!(!spec.shared);
        assert_eq!(spec.name, "billing-db", "default owned name");
        assert_eq!(spec.namespace, "billing-ns", "service ns by default");
        assert_eq!(spec.size, "10Gi");
    }

    #[test]
    fn db_unknown_env_falls_back_to_top_level() {
        let toml = r#"
            [database]
            engine = "postgres"
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "staging", "audit", "audit");
        assert!(!spec.shared);
        assert_eq!(spec.engine, DatabaseEngine::Postgres);
    }

    #[test]
    fn db_emits_url_and_password_secret() {
        let toml = r#"
            [database]
            engine = "postgres"
            shared = false
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "prod", "billing", "shop");
        let mut env = EmittedEnv::default();
        env.extend_database(&spec, "billing");
        assert_eq!(env.literals.len(), 1);
        assert_eq!(env.literals[0].0, "DATABASE_URL");
        assert!(env.literals[0].1.starts_with(
            "postgres://billing:$DATABASE_PASSWORD@billing-db.shop.svc.cluster.local:5432/billing"
        ));
        assert_eq!(env.from_secret, vec!["DATABASE_PASSWORD".to_string()]);
    }

    #[test]
    fn cache_shared_overlay() {
        let toml = r#"
            [cache]
            engine = "redis"
            shared = false

            [cache.dev]
            shared = true
            name = "redis"
            namespace = "shared-dev"
        "#;
        let raw: RawCache = toml::from_str::<toml::Value>(toml)
            .unwrap()
            .get("cache")
            .unwrap()
            .clone()
            .try_into()
            .unwrap();
        let spec = resolve_cache(&raw, "dev", "billing", "shop");
        assert!(spec.shared);
        assert_eq!(spec.name, "redis");
        assert_eq!(spec.namespace, "shared-dev");
        assert_eq!(
            spec.url(),
            "redis://redis.shared-dev.svc.cluster.local:6379"
        );
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

    #[test]
    fn migrations_sqlx_command_default() {
        let raw = RawMigrations {
            tool: "sqlx".into(),
            dir: default_migrations_dir(),
            run_on: default_run_on(),
            command: None,
        };
        let spec = resolve_migrations(&raw);
        assert_eq!(spec.tool, MigrationTool::Sqlx);
        assert_eq!(spec.run_on, MigrationRunOn::InitContainer);
        assert_eq!(
            spec.command,
            vec!["sqlx", "migrate", "run", "--source", "migrations/"]
        );
    }

    #[test]
    fn migrations_custom_requires_command() {
        let raw = RawMigrations {
            tool: "custom".into(),
            dir: "migrations/".into(),
            run_on: "init-container".into(),
            command: Some(vec!["./migrate.sh".into(), "--all".into()]),
        };
        let spec = resolve_migrations(&raw);
        assert_eq!(spec.tool, MigrationTool::Custom);
        assert_eq!(spec.command, vec!["./migrate.sh", "--all"]);
    }

    #[test]
    fn env_selection_precedence() {
        // explicit arg wins over TONIN_ENV. set_var / remove_var are unsafe
        // in edition 2024 (thread-safety hazard); this test is single-threaded
        // and the global env is the unit under test, so the unsafe blocks are
        // contained here.
        unsafe { std::env::set_var("TONIN_ENV", "staging") };
        assert_eq!(select_env(Some("prod")), "prod");
        // env var wins over default
        assert_eq!(select_env(None), "staging");
        unsafe { std::env::remove_var("TONIN_ENV") };
        assert_eq!(select_env(None), "dev");
    }
}
