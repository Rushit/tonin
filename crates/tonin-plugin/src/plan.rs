//! Plan: typed deployment description loaded from `tonin.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::stateful::{
    self, CacheSpec, ConfigSpec, DatabaseSpec, EmittedEnv, MigrationsSpec, RawCache, RawCallers,
    RawConfigBlock, RawDatabase, RawMigrations, RawSecrets, SecretsSpec,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("parsing {0}: {1}")]
    Toml(PathBuf, #[source] toml::de::Error),
    #[error(
        "{path}: schema = {found:?} is not supported by this CLI. \
         Supported schemas: {supported:?}. \
         Upgrade the CLI, or set `schema = \"{current}\"` at the top of tonin.toml."
    )]
    UnsupportedSchema {
        path: PathBuf,
        found: String,
        supported: Vec<String>,
        current: String,
    },
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mesh {
    #[default]
    Cilium,
    Istio,
    Linkerd,
    None,
}

impl Mesh {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mesh::Cilium => "cilium",
            Mesh::Istio => "istio",
            Mesh::Linkerd => "linkerd",
            Mesh::None => "none",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServiceRef {
    pub name: String,
    pub namespace: String,
}

impl ServiceRef {
    pub fn identity(&self) -> String {
        format!("{}.{}", self.name, self.namespace)
    }
}

// ---------- on-disk TOML shape ----------

/// The schema version this CLI knows how to read.
pub const CURRENT_SCHEMA: &str = "v1";
pub const SUPPORTED_SCHEMAS: &[&str] = &["v1"];

/// Minimum `tonin` CLI version that can fully render all `tonin.toml`
/// features exposed by this version of `tonin-plugin`.
///
/// The CLI checks this at `tonin k8s generate` / `tonin helm generate` time
/// and emits a warning (never an error) when it is older. Services continue
/// to work — the check is advisory so teams can upgrade at their own pace.
///
/// Bump this constant (in the same commit) whenever a new `tonin.toml`
/// section or field is added that older CLI versions would silently ignore.
pub const RECOMMENDED_CLI_MIN: &str = "0.5.0";

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    schema: Option<String>,
    service: RawService,
    deploy: RawDeploy,
    resources: RawResources,
    #[serde(default)]
    autoscale: Option<RawAutoscale>,
    #[serde(default)]
    depends_on: BTreeMap<String, String>,
    #[serde(default)]
    callers: RawCallers,
    #[serde(default)]
    database: Option<RawDatabase>,
    #[serde(default)]
    databases: std::collections::BTreeMap<String, RawDatabase>,
    #[serde(default)]
    cache: Option<RawCache>,
    #[serde(default)]
    caches: std::collections::BTreeMap<String, RawCache>,
    #[serde(default)]
    secrets: Option<RawSecrets>,
    #[serde(default)]
    migrations: Option<RawMigrations>,
    #[serde(default)]
    config: Option<RawConfigBlock>,
    #[serde(default)]
    client: Option<RawClientConfig>,
}

#[derive(Debug, Deserialize)]
struct RawService {
    name: String,
    version: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    web_mode: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    codec: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDeploy {
    replicas: u32,
    #[serde(default)]
    mesh: Option<Mesh>,
    #[serde(default = "default_true")]
    mcp_sidecar: bool,
    namespace: String,
    #[serde(default)]
    expose: Option<String>,
    #[serde(default, flatten)]
    envs: std::collections::BTreeMap<String, RawDeployEnv>,
}

#[derive(Debug, Deserialize, Default)]
struct RawDeployEnv {
    #[serde(default)]
    replicas: Option<u32>,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    mesh: Option<Mesh>,
    #[serde(default)]
    mcp_sidecar: Option<bool>,
    #[serde(default)]
    expose: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawResources {
    cpu: String,
    memory: String,
}

#[derive(Debug, Deserialize)]
struct RawAutoscale {
    max_replicas: u32,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
struct RawClientConfig {
    #[serde(default = "default_true")]
    coalesce: bool,
    #[serde(default)]
    cache: std::collections::BTreeMap<String, RawMethodCacheConfig>,
}

#[derive(Debug, Deserialize)]
struct RawMethodCacheConfig {
    ttl_ms: u64,
    #[serde(default = "default_cache_capacity")]
    capacity: usize,
}

fn default_cache_capacity() -> usize {
    1_000
}

#[derive(Clone, Debug, Serialize)]
pub struct MethodCacheSpec {
    pub ttl_ms: u64,
    pub capacity: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ClientSpec {
    pub coalesce: bool,
    pub caches: Vec<(String, MethodCacheSpec)>,
}

impl Default for ClientSpec {
    fn default() -> Self {
        Self {
            coalesce: true,
            caches: Vec::new(),
        }
    }
}

// ---------- normalized Plan ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Backend,
    Web,
}

impl ServiceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceKind::Backend => "backend",
            ServiceKind::Web => "web",
        }
    }
    pub fn is_web(&self) -> bool {
        matches!(self, ServiceKind::Web)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WebMode {
    Spa,
    Bff,
}

impl WebMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            WebMode::Spa => "spa",
            WebMode::Bff => "bff",
        }
    }
    pub fn container_port(&self) -> u32 {
        match self {
            WebMode::Spa => 8080,
            WebMode::Bff => 3000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub name: String,
    pub version: String,
    pub language: String,
    pub kind: ServiceKind,
    pub web_mode: Option<WebMode>,
    pub namespace: String,
    pub mesh: Mesh,
    pub replicas: u32,
    pub max_replicas: u32,
    pub mcp_sidecar: bool,
    pub expose: Option<String>,
    pub cpu: String,
    pub memory: String,
    pub image: String,
    pub depends_on: Vec<ServiceRef>,
    pub callers: Vec<ServiceRef>,
    pub dir: PathBuf,
    pub database: Option<DatabaseSpec>,
    pub named_databases: Vec<(String, DatabaseSpec)>,
    pub cache: Option<CacheSpec>,
    pub named_caches: Vec<(String, CacheSpec)>,
    pub secrets: Option<SecretsSpec>,
    pub migrations: Option<MigrationsSpec>,
    pub config: Option<ConfigSpec>,
    pub emitted_env: EmittedEnv,
    pub selected_env: String,
    pub client: ClientSpec,
}

impl Plan {
    pub fn load(toml_path: &Path) -> Result<Self, Error> {
        Self::load_with_env(toml_path, &stateful::select_env(None))
    }

    pub fn load_with_env(toml_path: &Path, env: &str) -> Result<Self, Error> {
        let raw_str = std::fs::read_to_string(toml_path)
            .map_err(|e| Error::Io(toml_path.to_path_buf(), e))?;
        let raw: RawConfig =
            toml::from_str(&raw_str).map_err(|e| Error::Toml(toml_path.to_path_buf(), e))?;

        if let Some(v) = raw.schema.as_deref()
            && !SUPPORTED_SCHEMAS.contains(&v)
        {
            return Err(Error::UnsupportedSchema {
                path: toml_path.to_path_buf(),
                found: v.to_string(),
                supported: SUPPORTED_SCHEMAS.iter().map(|s| s.to_string()).collect(),
                current: CURRENT_SCHEMA.to_string(),
            });
        }

        let depends_on: Vec<ServiceRef> = raw
            .depends_on
            .into_iter()
            .map(|(name, namespace)| ServiceRef { name, namespace })
            .collect();

        let explicit_callers = stateful::resolve_callers(&raw.callers, env);

        let deploy_overlay = raw.deploy.envs.get(env);
        let deploy_replicas = deploy_overlay
            .and_then(|o| o.replicas)
            .unwrap_or(raw.deploy.replicas);
        let deploy_namespace = deploy_overlay
            .and_then(|o| o.namespace.clone())
            .unwrap_or(raw.deploy.namespace);
        let deploy_mesh = deploy_overlay
            .and_then(|o| o.mesh)
            .or(raw.deploy.mesh)
            .unwrap_or_default();
        let deploy_mcp_sidecar = deploy_overlay
            .and_then(|o| o.mcp_sidecar)
            .unwrap_or(raw.deploy.mcp_sidecar);
        let deploy_expose = deploy_overlay
            .and_then(|o| o.expose.clone())
            .or(raw.deploy.expose);

        let max_replicas = raw
            .autoscale
            .as_ref()
            .map(|a| a.max_replicas)
            .unwrap_or(deploy_replicas);

        let dir = toml_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let image = std::env::var("TONIN_IMAGE_PREFIX")
            .map(|prefix| format!("{prefix}/{}:{}", raw.service.name, raw.service.version))
            .unwrap_or_else(|_| format!("micro/{}:{}", raw.service.name, raw.service.version));

        let kind = match raw.service.kind.as_deref() {
            Some("web") => ServiceKind::Web,
            _ => ServiceKind::Backend,
        };
        let web_mode = match (kind, raw.service.web_mode.as_deref()) {
            (ServiceKind::Web, Some("bff")) => Some(WebMode::Bff),
            (ServiceKind::Web, _) => Some(WebMode::Spa),
            _ => None,
        };

        let svc_name = raw.service.name.clone();
        let svc_ns = deploy_namespace.clone();
        let database = raw
            .database
            .as_ref()
            .map(|r| stateful::resolve_database(r, env, &svc_name, &svc_ns));
        let named_databases: Vec<(String, DatabaseSpec)> = raw
            .databases
            .iter()
            .map(|(name, r)| {
                (
                    name.clone(),
                    stateful::resolve_database(r, env, &svc_name, &svc_ns),
                )
            })
            .collect();
        let cache = raw
            .cache
            .as_ref()
            .map(|r| stateful::resolve_cache(r, env, &svc_name, &svc_ns));
        let named_caches: Vec<(String, CacheSpec)> = raw
            .caches
            .iter()
            .map(|(name, r)| {
                (
                    name.clone(),
                    stateful::resolve_cache(r, env, &svc_name, &svc_ns),
                )
            })
            .collect();
        let secrets = raw.secrets.as_ref().map(stateful::resolve_secrets);
        let migrations = raw.migrations.as_ref().map(stateful::resolve_migrations);
        let config = raw.config.as_ref().map(stateful::resolve_config);

        let client = raw
            .client
            .map(|c| {
                let mut caches: Vec<(String, MethodCacheSpec)> = c
                    .cache
                    .into_iter()
                    .map(|(method, mc)| {
                        (
                            method,
                            MethodCacheSpec {
                                ttl_ms: mc.ttl_ms,
                                capacity: mc.capacity,
                            },
                        )
                    })
                    .collect();
                caches.sort_by(|a, b| a.0.cmp(&b.0));
                ClientSpec {
                    coalesce: c.coalesce,
                    caches,
                }
            })
            .unwrap_or_default();

        let mut emitted_env = EmittedEnv::default();
        if let Some(d) = &database {
            emitted_env.extend_database(d, &svc_name);
        }
        for (name, d) in &named_databases {
            let prefix = format!("{}_DATABASE", name.to_uppercase());
            emitted_env.extend_database_named(&prefix, d, &svc_name);
        }
        if let Some(c) = &cache {
            emitted_env.extend_cache(c);
        }
        for (name, c) in &named_caches {
            let prefix = format!("{}_REDIS", name.to_uppercase());
            emitted_env.extend_cache_named(&prefix, c);
        }
        if let Some(s) = &secrets {
            emitted_env.extend_secrets(s);
        }

        Ok(Plan {
            name: raw.service.name,
            version: raw.service.version,
            language: raw.service.language.unwrap_or_else(|| "rust".into()),
            kind,
            web_mode,
            namespace: deploy_namespace,
            mesh: deploy_mesh,
            replicas: deploy_replicas,
            max_replicas,
            mcp_sidecar: deploy_mcp_sidecar,
            expose: deploy_expose,
            cpu: raw.resources.cpu,
            memory: raw.resources.memory,
            image,
            depends_on,
            callers: explicit_callers,
            dir,
            database,
            named_databases,
            cache,
            named_caches,
            secrets,
            migrations,
            config,
            client,
            emitted_env,
            selected_env: env.to_string(),
        })
    }

    pub fn load_workspace(root: &Path) -> Result<Vec<Plan>, Error> {
        Self::load_workspace_with_env(root, &stateful::select_env(None))
    }

    pub fn load_workspace_with_env(root: &Path, env: &str) -> Result<Vec<Plan>, Error> {
        let mut plans: Vec<Plan> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() == "tonin.toml")
            .map(|e| Plan::load_with_env(e.path(), env))
            .collect::<Result<_, _>>()?;

        let snapshot: Vec<(String, String, Vec<ServiceRef>)> = plans
            .iter()
            .map(|p| (p.name.clone(), p.namespace.clone(), p.depends_on.clone()))
            .collect();
        for plan in plans.iter_mut() {
            for (caller_name, caller_ns, deps) in &snapshot {
                if deps
                    .iter()
                    .any(|d| d.name == plan.name && d.namespace == plan.namespace)
                {
                    plan.callers.push(ServiceRef {
                        name: caller_name.clone(),
                        namespace: caller_ns.clone(),
                    });
                }
            }
            plan.callers.sort();
            plan.callers.dedup();
        }

        plans.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(plans)
    }
}
