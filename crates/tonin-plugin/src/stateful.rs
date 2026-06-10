//! Stateful dependencies (Phase 1 of stateful-deps design).
//!
//! Loads `[database]`, `[cache]`, `[secrets]`, `[migrations]` from
//! `tonin.toml`, applies `[database.dev]` / `[database.prod]` env-overlay,
//! and normalizes into types the renderer will consume.

use serde::{Deserialize, Serialize};

// ---------- on-disk TOML shape ----------

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawDatabase {
    pub engine: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub shared: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
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
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawCache {
    pub engine: String,
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
    #[serde(default)]
    pub url: Option<String>,
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

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawMigrations {
    pub tool: String,
    #[serde(default = "default_migrations_dir")]
    pub dir: String,
    #[serde(default = "default_run_on")]
    pub run_on: String,
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

#[derive(Clone, Debug)]
pub struct DatabaseSpec {
    pub engine: DatabaseEngine,
    pub version: String,
    pub size: String,
    pub shared: bool,
    pub name: String,
    pub namespace: String,
    pub url_override: Option<String>,
}

fn default_db_version(engine: DatabaseEngine) -> String {
    match engine {
        DatabaseEngine::Postgres => "18".into(),
        DatabaseEngine::Mysql => "8".into(),
        DatabaseEngine::Clickhouse => "24.3".into(),
        DatabaseEngine::Sqlite | DatabaseEngine::None => "latest".into(),
    }
}

impl DatabaseSpec {
    pub fn image(&self) -> String {
        match self.engine {
            DatabaseEngine::Postgres => format!("postgres:{}", self.version),
            DatabaseEngine::Mysql => format!("mysql:{}", self.version),
            DatabaseEngine::Clickhouse => format!("clickhouse/clickhouse-server:{}", self.version),
            DatabaseEngine::Sqlite | DatabaseEngine::None => "".into(),
        }
    }
    pub fn host(&self) -> String {
        format!("{}.{}.svc.cluster.local", self.name, self.namespace)
    }
    pub fn port(&self) -> u32 {
        self.engine.default_port()
    }
    pub fn url_template(&self, service_name: &str) -> String {
        if let Some(ref url) = self.url_override {
            return url.clone();
        }
        format!(
            "{}://{svc}:$DATABASE_PASSWORD@{host}:{port}/{svc}",
            self.engine.as_str(),
            svc = service_name,
            host = self.host(),
            port = self.port(),
        )
    }
}

#[derive(Clone, Debug)]
pub struct CacheSpec {
    pub engine: CacheEngine,
    pub size: String,
    pub shared: bool,
    pub name: String,
    pub namespace: String,
    pub url_override: Option<String>,
}

impl CacheSpec {
    pub fn host(&self) -> String {
        format!("{}.{}.svc.cluster.local", self.name, self.namespace)
    }
    pub fn port(&self) -> u32 {
        self.engine.default_port()
    }
    pub fn url(&self) -> String {
        if let Some(ref url) = self.url_override {
            return url.clone();
        }
        format!("redis://{}:{}", self.host(), self.port())
    }
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
    pub run_on: MigrationRunOn,
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
                base.insert(key.clone(), ns.clone());
            }
            RawCallerEntry::Env(map) if key == env => {
                overlay = map.clone();
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
    let name = overlay
        .and_then(|o| o.name.clone())
        .or_else(|| raw.name.clone())
        .unwrap_or_else(|| format!("{}-db", service_name));
    let namespace = overlay
        .and_then(|o| o.namespace.clone())
        .or_else(|| raw.namespace.clone())
        .unwrap_or_else(|| service_namespace.to_string());
    let url_override = overlay.and_then(|o| o.url.clone());
    DatabaseSpec {
        engine,
        version,
        size,
        shared,
        name,
        namespace,
        url_override,
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
    let url_override = overlay.and_then(|o| o.url.clone());
    CacheSpec {
        engine,
        size,
        shared,
        name,
        namespace,
        url_override,
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
    MigrationsSpec {
        tool,
        dir: raw.dir.clone(),
        run_on,
        command,
    }
}

// ---------- emitted env vars ----------

#[derive(Clone, Debug, Default)]
pub struct EmittedEnv {
    pub literals: Vec<(String, String)>,
    pub from_secret: Vec<String>,
}

impl EmittedEnv {
    pub fn extend_database(&mut self, spec: &DatabaseSpec, service_name: &str) {
        self.extend_database_named("DATABASE", spec, service_name);
    }

    pub fn extend_database_named(
        &mut self,
        var_prefix: &str,
        spec: &DatabaseSpec,
        service_name: &str,
    ) {
        if matches!(spec.engine, DatabaseEngine::None) {
            return;
        }
        self.literals
            .push((format!("{var_prefix}_URL"), spec.url_template(service_name)));
        if spec.url_override.is_none() {
            self.from_secret.push(format!("{var_prefix}_PASSWORD"));
        }
    }

    pub fn extend_cache(&mut self, spec: &CacheSpec) {
        self.extend_cache_named("REDIS", spec);
    }

    pub fn extend_cache_named(&mut self, var_prefix: &str, spec: &CacheSpec) {
        if matches!(spec.engine, CacheEngine::None) {
            return;
        }
        self.literals
            .push((format!("{var_prefix}_URL"), spec.url()));
    }

    pub fn extend_secrets(&mut self, spec: &SecretsSpec) {
        for key in &spec.required {
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
        assert!(!spec.shared);
        assert_eq!(spec.name, "billing-db");
        assert_eq!(spec.namespace, "billing-ns");
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
            gateway         = "agnitiv"
            zradar-platform = "agnitiv"
        "#,
        );
        let callers = resolve_callers(&raw, "dev");
        assert_eq!(callers.len(), 2);
        assert!(callers.iter().all(|c| c.namespace == "agnitiv"));
    }

    #[test]
    fn callers_dev_overlay_overrides_namespace() {
        let raw = parse_callers(
            r#"
            [callers]
            gateway         = "agnitiv"
            zradar-platform = "agnitiv"

            [callers.dev]
            gateway         = "agnitiv-dev"
            zradar-platform = "agnitiv-dev"
        "#,
        );
        let dev = resolve_callers(&raw, "dev");
        assert!(
            dev.iter().all(|c| c.namespace == "agnitiv-dev"),
            "dev overlay must win"
        );
        let prod = resolve_callers(&raw, "prod");
        assert!(
            prod.iter().all(|c| c.namespace == "agnitiv"),
            "prod falls back to base"
        );
    }

    #[test]
    fn callers_dev_overlay_adds_new_caller() {
        let raw = parse_callers(
            r#"
            [callers]
            gateway = "agnitiv"

            [callers.dev]
            gateway    = "agnitiv-dev"
            debug-tool = "agnitiv-dev"
        "#,
        );
        let dev = resolve_callers(&raw, "dev");
        assert_eq!(dev.len(), 2, "overlay adds debug-tool");
        let prod = resolve_callers(&raw, "prod");
        assert_eq!(prod.len(), 1, "prod sees base only");
    }

    #[test]
    fn db_dev_url_override_used_verbatim() {
        let toml = r#"
            [database]
            engine = "postgres"

            [database.dev]
            shared = true
            url    = "postgresql://postgres:postgres@shared.svc:5432/mydb"
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "dev", "identity", "agnitiv");
        assert_eq!(
            spec.url_template("identity"),
            "postgresql://postgres:postgres@shared.svc:5432/mydb"
        );
        assert!(spec.url_override.is_some());
    }

    #[test]
    fn db_prod_no_url_override_keeps_template() {
        let toml = r#"
            [database]
            engine = "postgres"

            [database.dev]
            shared = true
            url    = "postgresql://postgres:postgres@shared.svc:5432/mydb"
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "prod", "identity", "agnitiv");
        assert!(spec.url_override.is_none());
        assert!(
            spec.url_template("identity").contains("$DATABASE_PASSWORD"),
            "prod uses password-template URL"
        );
    }

    #[test]
    fn emitted_env_skips_password_when_url_override_set() {
        let toml = r#"
            [database]
            engine = "postgres"

            [database.dev]
            shared = true
            url    = "postgresql://postgres:postgres@shared.svc:5432/mydb"
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "dev", "identity", "agnitiv");
        let mut env = EmittedEnv::default();
        env.extend_database(&spec, "identity");
        assert!(
            !env.from_secret.iter().any(|s| s == "DATABASE_PASSWORD"),
            "url_override must suppress DATABASE_PASSWORD secret injection"
        );
        assert!(
            env.literals
                .iter()
                .any(|(k, v)| k == "DATABASE_URL" && v.contains("shared.svc"))
        );
    }

    #[test]
    fn emitted_env_injects_password_when_no_url_override() {
        let toml = r#"
            [database]
            engine = "postgres"
        "#;
        let raw = toml_to_raw_db(toml);
        let spec = resolve_database(&raw, "prod", "identity", "agnitiv");
        let mut env = EmittedEnv::default();
        env.extend_database(&spec, "identity");
        assert!(env.from_secret.iter().any(|s| s == "DATABASE_PASSWORD"));
    }

    #[test]
    fn named_database_emits_prefixed_vars() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            databases: std::collections::BTreeMap<String, RawDatabase>,
        }
        let toml = r#"
            [databases.write]
            engine = "postgres"

            [databases.write.dev]
            shared = true
            url = "postgresql://postgres:postgres@shared.svc:5432/app_dev"
        "#;
        let w: Wrapper = toml::from_str(toml).unwrap();
        let spec = resolve_database(w.databases.get("write").unwrap(), "dev", "app", "agnitiv");
        let mut env = EmittedEnv::default();
        env.extend_database_named("WRITE_DATABASE", &spec, "app");
        assert!(env.literals.iter().any(|(k, _)| k == "WRITE_DATABASE_URL"));
        assert!(
            !env.from_secret
                .iter()
                .any(|s| s == "WRITE_DATABASE_PASSWORD")
        );
    }

    #[test]
    fn named_cache_emits_prefixed_var() {
        let toml = r#"
            [cache]
            engine = "redis"

            [cache.dev]
            shared = true
            url    = "redis://redis.shared-dev.svc:6379"
        "#;
        let raw: RawCache = toml::from_str::<toml::Value>(toml)
            .unwrap()
            .get("cache")
            .unwrap()
            .clone()
            .try_into()
            .unwrap();
        let spec = resolve_cache(&raw, "dev", "identity", "agnitiv");
        let mut env = EmittedEnv::default();
        env.extend_cache_named("SESSION_REDIS", &spec);
        assert!(
            env.literals
                .iter()
                .any(|(k, v)| k == "SESSION_REDIS_URL" && v.contains("shared-dev"))
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
