//! Plan: typed deployment description loaded from `tonin.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::stateful::{
    self, CacheSpec, ConfigSpec, DatabaseSpec, EmittedEnv, MigrationsSpec, RawCache,
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

// ---------- on-disk TOML shape (what users edit) ----------

/// The schema version this CLI knows how to read. Today there is exactly
/// one: `"v1"`. New TOML files written by `tonin service new` always
/// stamp `schema = "v1"` at the top.
///
/// ## Backward compatibility policy
///
/// `tonin.toml` is backward-compatible **by default**:
///
/// - A file without a `schema` key parses as v1 (no warning) so files
///   written before this field existed keep working untouched.
/// - A file with `schema = "v1"` parses as v1.
/// - A file with an unknown schema (e.g. a file from a newer CLI that
///   added "v2") returns a clear error directing the user to upgrade
///   the CLI or downgrade the file.
///
/// New fields added to v1 must stay **additive** — optional with a sensible
/// default. Removing or renaming a field, or changing its type, requires
/// bumping to a new schema version and shipping a migration recipe in the
/// same release. Coding agents editing this code: never make a v1 field
/// removal or rename without bumping the version constant below.
pub const CURRENT_SCHEMA: &str = "v1";
pub const SUPPORTED_SCHEMAS: &[&str] = &["v1"];

#[derive(Debug, Deserialize)]
struct RawConfig {
    /// Schema version of this `tonin.toml`. Optional today (missing →
    /// treated as `"v1"`); will become a deprecation warning once `v2`
    /// ships, and recommended-on-write from then on. See [`CURRENT_SCHEMA`].
    #[serde(default)]
    schema: Option<String>,
    service: RawService,
    deploy: RawDeploy,
    resources: RawResources,
    #[serde(default)]
    autoscale: Option<RawAutoscale>,
    /// `[depends_on]` map: `<name> = "<namespace>"`.
    #[serde(default)]
    depends_on: BTreeMap<String, String>,
    // -- Stateful dependencies (Phase 1) --
    #[serde(default)]
    database: Option<RawDatabase>,
    #[serde(default)]
    cache: Option<RawCache>,
    #[serde(default)]
    secrets: Option<RawSecrets>,
    #[serde(default)]
    migrations: Option<RawMigrations>,
    /// Dynamic application config (env / etcd / github / chained).
    #[serde(default)]
    config: Option<RawConfigBlock>,
    /// Generated client behaviour (coalescing + optional TTL cache).
    /// Entirely optional — absence preserves today's behaviour.
    #[serde(default)]
    client: Option<RawClientConfig>,
}

#[derive(Debug, Deserialize)]
struct RawService {
    name: String,
    version: String,
    #[serde(default)]
    language: Option<String>,
    /// "backend" (default) or "web". Web skips MCP and renders Ingress.
    #[serde(default, rename = "type")]
    kind: Option<String>,
    /// Only valid when kind=web: "spa" (default) | "bff".
    #[serde(default)]
    web_mode: Option<String>,
    /// Parsed but not yet consumed — buffa codegen path lands in a later phase.
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
    expose: Option<String>, // "ingress" | "clusterip"
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

// ---------- [client] config (additive, v1-compatible) ----------

/// Raw `[client]` block from `tonin.toml`. All fields optional so omitting
/// the section entirely preserves today's default behaviour (coalescing on,
/// no TTL cache).
///
/// Example:
/// ```toml
/// [client]
/// coalesce = true   # default — may be set to false to opt out
///
/// [client.cache.Check]
/// ttl_ms   = 500
/// capacity = 1000
/// ```
#[derive(Debug, Default, Deserialize)]
struct RawClientConfig {
    /// Enable request coalescing for generated clients. Default: `true`.
    #[serde(default = "default_true")]
    coalesce: bool,
    /// Per-method TTL cache settings. Key = gRPC method name (e.g. `"Check"`).
    #[serde(default)]
    cache: std::collections::BTreeMap<String, RawMethodCacheConfig>,
}

#[derive(Debug, Deserialize)]
struct RawMethodCacheConfig {
    /// How long (milliseconds) an `Ok` response is considered fresh.
    ttl_ms: u64,
    /// Maximum number of cached entries for this method. Default: 1 000.
    #[serde(default = "default_cache_capacity")]
    capacity: usize,
}

fn default_cache_capacity() -> usize {
    1_000
}

/// Resolved per-method cache config (milliseconds → `Duration` already done).
#[derive(Clone, Debug, Serialize)]
pub struct MethodCacheSpec {
    pub ttl_ms: u64,
    pub capacity: usize,
}

/// Resolved `[client]` config passed to the renderer.
#[derive(Clone, Debug, Serialize)]
pub struct ClientSpec {
    pub coalesce: bool,
    /// Sorted for deterministic template output.
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

// ---------- normalized Plan (what the renderer consumes) ----------

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
    /// Vite + React SPA, served as static files by nginx (port 8080).
    Spa,
    /// Next.js Backend-for-Frontend, single Node container (port 3000).
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
    /// Some(_) iff kind == Web.
    pub web_mode: Option<WebMode>,
    pub namespace: String,
    pub mesh: Mesh,
    pub replicas: u32,
    pub max_replicas: u32,
    pub mcp_sidecar: bool,
    pub expose: Option<String>,
    pub cpu: String,
    pub memory: String,
    /// Image tag the CLI will deploy. Computed from name+version; users
    /// override via env at render time.
    pub image: String,
    /// Services this one calls.
    pub depends_on: Vec<ServiceRef>,
    /// Services that call this one. Computed by the workspace loader.
    pub callers: Vec<ServiceRef>,
    /// Path to the service directory.
    pub dir: PathBuf,
    // -- Stateful dependencies (Phase 1) --
    /// Resolved DB spec for the selected env (None if no `[database]` block).
    pub database: Option<DatabaseSpec>,
    /// Resolved cache spec for the selected env.
    pub cache: Option<CacheSpec>,
    pub secrets: Option<SecretsSpec>,
    pub migrations: Option<MigrationsSpec>,
    /// Resolved dynamic-config spec (None if no `[config]` block; falls back
    /// to `EnvConfig` at runtime).
    pub config: Option<ConfigSpec>,
    /// Env vars the deployment template must emit on the service container,
    /// derived from the above. Phase 2 (renderer) consumes this.
    pub emitted_env: EmittedEnv,
    /// Which env was selected when resolving overlays (for diagnostics).
    pub selected_env: String,
    /// Resolved `[client]` config for generated client code.
    pub client: ClientSpec,
}

impl Plan {
    /// Load a single service plan from its `tonin.toml`.
    /// Uses `select_env(None)` to pick the env (CLI flag > `TONIN_ENV` > `"dev"`).
    pub fn load(toml_path: &Path) -> Result<Self, Error> {
        Self::load_with_env(toml_path, &stateful::select_env(None))
    }

    /// Load a single service plan, explicitly choosing which env's overlays
    /// to apply. `callers` is left empty; use `load_workspace_with_env` to
    /// populate it.
    pub fn load_with_env(toml_path: &Path, env: &str) -> Result<Self, Error> {
        let raw_str = std::fs::read_to_string(toml_path)
            .map_err(|e| Error::Io(toml_path.to_path_buf(), e))?;
        let raw: RawConfig =
            toml::from_str(&raw_str).map_err(|e| Error::Toml(toml_path.to_path_buf(), e))?;

        // Schema gate. Missing → treated as the current schema for
        // backward compatibility with files written before this field
        // existed. Unknown → hard error directing the user to upgrade.
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

        let depends_on = raw
            .depends_on
            .into_iter()
            .map(|(name, namespace)| ServiceRef { name, namespace })
            .collect();

        let max_replicas = raw
            .autoscale
            .as_ref()
            .map(|a| a.max_replicas)
            .unwrap_or(raw.deploy.replicas);

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
            (ServiceKind::Web, _) => Some(WebMode::Spa), // default for web
            _ => None,
        };

        // Resolve stateful deps for the selected env.
        let svc_name = raw.service.name.clone();
        let svc_ns = raw.deploy.namespace.clone();
        let database = raw
            .database
            .as_ref()
            .map(|r| stateful::resolve_database(r, env, &svc_name, &svc_ns));
        let cache = raw
            .cache
            .as_ref()
            .map(|r| stateful::resolve_cache(r, env, &svc_name, &svc_ns));
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
                // Stable ordering for deterministic template output.
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
        if let Some(c) = &cache {
            emitted_env.extend_cache(c);
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
            namespace: raw.deploy.namespace,
            mesh: raw.deploy.mesh.unwrap_or_default(),
            replicas: raw.deploy.replicas,
            max_replicas,
            mcp_sidecar: raw.deploy.mcp_sidecar,
            expose: raw.deploy.expose,
            cpu: raw.resources.cpu,
            memory: raw.resources.memory,
            image,
            depends_on,
            callers: Vec::new(),
            dir,
            database,
            cache,
            secrets,
            migrations,
            config,
            client,
            emitted_env,
            selected_env: env.to_string(),
        })
    }

    /// Load every `tonin.toml` under `root` and link the depends_on graph
    /// in both directions.
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

        // Compute inverse depends_on (callers).
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
