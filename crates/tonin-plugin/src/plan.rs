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
    #[error("depends_on.{name}: {reason}")]
    InvalidDependency { name: String, reason: String },
    #[error(
        "{context}: namespace {value:?} has an unresolved placeholder \
         (only `{{env}}` is supported)"
    )]
    UnresolvedNamespace { context: String, value: String },
    #[error("[env].{key}: {reason}")]
    InvalidEnvVar { key: String, reason: String },
    #[error(
        "[env].{key} collides with the {origin} env var of the same name that \
         tonin already emits — rename the [env] key or drop the duplicate"
    )]
    ReservedEnvKey { key: String, origin: String },
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

/// Default egress port for a `[depends_on]` entry — the tonin gRPC listen
/// convention. Matches the port the generated network policy hardcoded before
/// ports were declarable, so bare-string dependencies render identically.
pub const DEFAULT_DEPENDENCY_PORT: u32 = 50051;

fn default_dependency_port() -> u32 {
    DEFAULT_DEPENDENCY_PORT
}

/// One resolved `[depends_on]` entry: the downstream service, the namespace it
/// lives in for the selected environment, and the port egress is allowed on.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct DependencyRef {
    pub name: String,
    pub namespace: String,
    /// Dependency's listen port (`port` in the table form). Defaults to
    /// [`DEFAULT_DEPENDENCY_PORT`] so the bare-string shorthand keeps
    /// rendering today's egress rule.
    #[serde(default = "default_dependency_port")]
    pub port: u32,
}

/// One `[depends_on]` entry parsed from TOML, before environment resolution.
///
/// Both the shorthand (`name = "<ns>"`) and the table form
/// (`name = { namespace = "<ns>", port = <p>, <env> = "<ns>", envs = [..] }`)
/// land here.
struct DepSpec {
    /// Default namespace pattern (may contain `{env}`), if declared.
    namespace: Option<String>,
    /// Per-env namespace overrides (env name → pattern).
    env_overrides: BTreeMap<String, String>,
    /// Restrict the dependency to these envs; `None` ⇒ every env.
    envs: Option<Vec<String>>,
    /// Declared egress port; `None` ⇒ [`DEFAULT_DEPENDENCY_PORT`].
    port: Option<u32>,
}

/// Substitute the `{env}` placeholder in a namespace pattern.
pub(crate) fn apply_env(pattern: &str, env: &str) -> String {
    pattern.replace("{env}", env)
}

/// Reject a namespace that still carries an unresolved `{...}` placeholder.
/// `{env}` is the only supported token, so anything left over is a typo.
fn ensure_resolved(context: &str, value: &str) -> Result<(), Error> {
    if value.contains('{') || value.contains('}') {
        return Err(Error::UnresolvedNamespace {
            context: context.to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}

/// Parse a single `[depends_on]` entry (string shorthand or table form).
fn parse_dependency(name: &str, value: toml::Value) -> Result<DepSpec, Error> {
    let invalid = |reason: String| Error::InvalidDependency {
        name: name.to_string(),
        reason,
    };
    match value {
        toml::Value::String(s) => Ok(DepSpec {
            namespace: Some(s),
            env_overrides: BTreeMap::new(),
            envs: None,
            port: None,
        }),
        toml::Value::Table(table) => {
            let mut namespace = None;
            let mut envs = None;
            let mut env_overrides = BTreeMap::new();
            let mut port = None;
            for (key, val) in table {
                match key.as_str() {
                    "namespace" => {
                        namespace = Some(
                            val.as_str()
                                .ok_or_else(|| invalid("`namespace` must be a string".into()))?
                                .to_string(),
                        );
                    }
                    "port" => {
                        let p = val
                            .as_integer()
                            .ok_or_else(|| invalid("`port` must be an integer".into()))?;
                        if !(1..=65535).contains(&p) {
                            return Err(invalid(format!("`port` {p} is outside 1-65535")));
                        }
                        port = Some(p as u32);
                    }
                    "envs" => {
                        let arr = val
                            .as_array()
                            .ok_or_else(|| invalid("`envs` must be an array of strings".into()))?;
                        let mut list = Vec::with_capacity(arr.len());
                        for item in arr {
                            list.push(
                                item.as_str()
                                    .ok_or_else(|| {
                                        invalid("`envs` must be an array of strings".into())
                                    })?
                                    .to_string(),
                            );
                        }
                        envs = Some(list);
                    }
                    // Any other key is a per-env namespace override (`prod = "..."`).
                    other => {
                        let ns = val.as_str().ok_or_else(|| {
                            invalid(format!("override `{other}` must be a namespace string"))
                        })?;
                        env_overrides.insert(other.to_string(), ns.to_string());
                    }
                }
            }
            Ok(DepSpec {
                namespace,
                env_overrides,
                envs,
                port,
            })
        }
        other => Err(invalid(format!(
            "expected a namespace string or a table, got {}",
            other.type_str()
        ))),
    }
}

/// Resolve `[depends_on]` for one environment into concrete egress targets.
///
/// - A dependency whose `envs` whitelist excludes `env` is dropped.
/// - The namespace is the per-env override if present, else the default,
///   with `{env}` substituted.
/// - `@inherit` drops the entry from rendered output (the namespace is
///   supplied at deploy time / by GitOps).
/// - An active dependency with no resolvable namespace — or one left with an
///   unresolved placeholder — is a hard error. There is no silent fallback to
///   a base value, which is what let a dev namespace leak into prod before.
fn resolve_depends_on(
    raw: BTreeMap<String, toml::Value>,
    env: &str,
) -> Result<Vec<DependencyRef>, Error> {
    let mut out = Vec::new();
    for (name, value) in raw {
        let spec = parse_dependency(&name, value)?;
        if let Some(envs) = &spec.envs
            && !envs.iter().any(|e| e == env)
        {
            continue; // not active in this environment
        }
        let Some(pattern) = spec.env_overrides.get(env).or(spec.namespace.as_ref()) else {
            return Err(Error::InvalidDependency {
                name: name.clone(),
                reason: format!(
                    "has no namespace for env '{env}' \
                     (set {name}.{env}, use \"{{env}}\", or mark \"@inherit\")"
                ),
            });
        };
        let resolved = apply_env(pattern, env);
        if resolved == "@inherit" {
            continue; // namespace owned by the deploy layer; omit egress entry
        }
        ensure_resolved(&format!("depends_on.{name}"), &resolved)?;
        if resolved.is_empty() {
            return Err(Error::InvalidDependency {
                name: name.clone(),
                reason: format!("namespace for env '{env}' is empty"),
            });
        }
        out.push(DependencyRef {
            name,
            namespace: resolved,
            port: spec.port.unwrap_or(DEFAULT_DEPENDENCY_PORT),
        });
    }
    Ok(out)
}

/// Resolve the optional `[env]` table (plain, non-secret runtime env vars)
/// for one environment.
///
/// - `KEY = "value"` string entries form the base set.
/// - `[env.<env>]` sub-tables override the base for that environment only.
/// - `{env}` in values is substituted with the environment being rendered
///   (keys are taken literally). Other `{...}` tokens pass through verbatim,
///   since env values may legitimately contain braces.
///
/// Returns a sorted list so rendered output is deterministic.
fn resolve_env_vars(
    raw: &BTreeMap<String, toml::Value>,
    env: &str,
) -> Result<Vec<(String, String)>, Error> {
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    // Base entries first ...
    for (key, value) in raw {
        match value {
            toml::Value::String(s) => {
                merged.insert(key.clone(), apply_env(s, env));
            }
            toml::Value::Table(_) => {} // per-env overlay, handled below
            other => {
                return Err(Error::InvalidEnvVar {
                    key: key.clone(),
                    reason: format!(
                        "must be a string (write PORT = \"8080\") or a per-env \
                         override table, got {}",
                        other.type_str()
                    ),
                });
            }
        }
    }
    // ... then the overlay for the selected env wins. Inactive env tables are
    // still type-checked so a typo fails in every environment, not just one.
    for (table_key, value) in raw {
        let toml::Value::Table(table) = value else {
            continue;
        };
        for (key, val) in table {
            let Some(s) = val.as_str() else {
                return Err(Error::InvalidEnvVar {
                    key: format!("{table_key}.{key}"),
                    reason: format!("must be a string, got {}", val.type_str()),
                });
            };
            if table_key == env {
                merged.insert(key.clone(), apply_env(s, env));
            }
        }
    }
    Ok(merged.into_iter().collect())
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
///
/// 0.6.0: per-environment namespaces and dependencies (`{env}` placeholders
/// and the Cargo-style `[depends_on]` table form) — a CLI older than this
/// can't render them.
///
/// 0.14.0: `[env]` / `[env.<env>]` plain env-var tables (older CLIs silently
/// ignore them) and the `port` field in the `[depends_on]` table form.
pub const RECOMMENDED_CLI_MIN: &str = "0.14.0";

#[derive(Debug, Deserialize)]
struct RawImage {
    #[serde(default)]
    registry: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSecurity {
    #[serde(default)]
    pod: Option<toml::Value>,
    #[serde(default)]
    container: Option<toml::Value>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawPathRule {
    pattern: String,
    #[serde(default)]
    propagate_to_callers: bool,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    build_order: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    schema: Option<String>,
    service: RawService,
    deploy: RawDeploy,
    resources: RawResources,
    #[serde(default)]
    autoscale: Option<RawAutoscale>,
    // String shorthand (`name = "<ns>"`) or table form
    // (`name = { namespace = "<ns>", port = <p>, <env> = "<ns>", envs = [..] }`).
    // Parsed as raw values and interpreted by `resolve_depends_on`.
    #[serde(default)]
    depends_on: BTreeMap<String, toml::Value>,
    // Plain (non-secret) runtime env vars: `KEY = "value"` string entries plus
    // `[env.<env>]` per-env override tables. Interpreted by `resolve_env_vars`.
    #[serde(default)]
    env: BTreeMap<String, toml::Value>,
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
    #[serde(default)]
    image: Option<RawImage>,
    #[serde(default)]
    security: Option<RawSecurity>,
    #[serde(default)]
    path_rules: Vec<RawPathRule>,
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
    /// Explicit listen port. Optional — defaults per kind when unset
    /// (web: 8080/3000 by mode, http: 8080, gRPC backend: 50051).
    #[serde(default)]
    port: Option<u32>,
    /// HTTP health-probe config (`[service.health]`).
    #[serde(default)]
    health: Option<RawHealth>,
    /// Additional HTTP endpoint (`[service.http]`). Lets a gRPC `backend` ALSO
    /// serve HTTP (health/metrics/admin) — the two are not mutually exclusive.
    #[serde(default)]
    http: Option<RawHttpEndpoint>,
}

#[derive(Debug, Deserialize)]
struct RawHealth {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    port: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawHttpEndpoint {
    port: u32,
    #[serde(default)]
    health_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDeploy {
    replicas: u32,
    #[serde(default)]
    mesh: Option<Mesh>,
    /// Explicit override for auto-derived MCP mode.
    /// `false` → force McpMode::None; `true` / absent → auto-derive.
    #[serde(default)]
    mcp_sidecar: Option<bool>,
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
    /// Escape hatch: `[client] proxy = false` disables auto-injected outbound proxy.
    #[serde(default)]
    proxy: Option<bool>,
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
    /// `Some(false)` = developer explicitly opted out of the auto-injected proxy.
    pub proxy_override: Option<bool>,
}

impl Default for ClientSpec {
    fn default() -> Self {
        Self {
            coalesce: true,
            caches: Vec::new(),
            proxy_override: None,
        }
    }
}

// ---------- normalized Plan ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Backend,
    Web,
    Http,
}

impl ServiceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceKind::Backend => "backend",
            ServiceKind::Web => "web",
            ServiceKind::Http => "http",
        }
    }
    pub fn is_web(&self) -> bool {
        matches!(self, ServiceKind::Web)
    }
    pub fn is_http(&self) -> bool {
        matches!(self, ServiceKind::Http)
    }
}

/// Resolved HTTP health-probe configuration (`[service.health]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HealthSpec {
    pub path: String,
    pub port: u32,
    /// `true` → emit a native Kubernetes `grpc:` probe on `port` (gRPC
    /// services); `false` → `httpGet` on `port` at `path`.
    pub grpc: bool,
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

/// How the MCP tool surface is served for this service.
///
/// Derived automatically from `[service].language` and `[service].type` —
/// never needs to be declared manually in `tonin.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpMode {
    /// MCP server runs in the same process as the gRPC server (Rust services).
    InProcess,
    /// MCP server runs as a separate sidecar container (Python, TypeScript, …).
    Sidecar,
    /// MCP is disabled — HTTP/web services have no gRPC surface to expose.
    None,
}

impl McpMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            McpMode::InProcess => "in-process",
            McpMode::Sidecar => "sidecar",
            McpMode::None => "none",
        }
    }
}

/// Free-form pod and container security context from `[security]` in `tonin.toml`.
///
/// Keys may be written in `snake_case` (tonin-helm converts to camelCase automatically)
/// or already in `camelCase` — both are passed through correctly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecuritySection {
    #[serde(default)]
    pub pod: Option<toml::Value>,
    #[serde(default)]
    pub container: Option<toml::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathRule {
    pub pattern: String,
    pub propagate_to_callers: bool,
    pub layer: Option<String>,
    pub packages: Vec<String>,
    pub build_order: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub name: String,
    pub version: String,
    pub language: String,
    pub kind: ServiceKind,
    pub web_mode: Option<WebMode>,
    /// Effective primary container/listen port (`[service].port` or per-kind default).
    pub port: u32,
    /// Additional HTTP port, when a gRPC backend also serves HTTP (`[service.http]`).
    pub http_port: Option<u32>,
    /// HTTP health probe, when an HTTP surface exists (always for http; opt-in otherwise).
    pub health: Option<HealthSpec>,
    pub namespace: String,
    pub mesh: Mesh,
    pub replicas: u32,
    pub max_replicas: u32,
    /// How MCP is served — derived from language + kind, never declared manually.
    pub mcp_mode: McpMode,
    /// Whether the outbound proxy sidecar is injected — derived from language + depends_on.
    pub needs_outbound_proxy: bool,
    pub expose: Option<String>,
    pub cpu: String,
    pub memory: String,
    pub image: String,
    pub image_registry: Option<String>,
    pub security: Option<SecuritySection>,
    pub depends_on: Vec<DependencyRef>,
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
    /// Plain env vars from `[env]` (+ `[env.<env>]` overlay), resolved for
    /// `selected_env` and sorted by key. Rendered into the chart's `env:` map.
    pub env_vars: Vec<(String, String)>,
    pub selected_env: String,
    pub client: ClientSpec,
    pub path_rules: Vec<PathRule>,
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

        let depends_on = resolve_depends_on(raw.depends_on, env)?;
        let env_vars = resolve_env_vars(&raw.env, env)?;

        let explicit_callers = stateful::resolve_callers(&raw.callers, env);

        let deploy_overlay = raw.deploy.envs.get(env);
        let deploy_replicas = deploy_overlay
            .and_then(|o| o.replicas)
            .unwrap_or(raw.deploy.replicas);
        let deploy_namespace = {
            let raw_ns = deploy_overlay
                .and_then(|o| o.namespace.clone())
                .unwrap_or(raw.deploy.namespace);
            let ns = apply_env(&raw_ns, env);
            ensure_resolved("deploy.namespace", &ns)?;
            ns
        };
        let deploy_mesh = deploy_overlay
            .and_then(|o| o.mesh)
            .or(raw.deploy.mesh)
            .unwrap_or_default();
        // env overlay wins over base; both are Option<bool>.
        let mcp_sidecar_override: Option<bool> = deploy_overlay
            .and_then(|o| o.mcp_sidecar)
            .or(raw.deploy.mcp_sidecar);
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

        // Priority: TONIN_IMAGE_PREFIX env var > [image].registry in tonin.toml > "micro/" fallback.
        let image_registry: Option<String> = std::env::var("TONIN_IMAGE_PREFIX")
            .ok()
            .or_else(|| raw.image.as_ref().and_then(|img| img.registry.clone()));
        let image = image_registry
            .as_deref()
            .map(|prefix| format!("{prefix}/{}:{}", raw.service.name, raw.service.version))
            .unwrap_or_else(|| format!("micro/{}:{}", raw.service.name, raw.service.version));
        let security = raw.security.map(|s| SecuritySection {
            pod: s.pod,
            container: s.container,
        });

        let kind = match raw.service.kind.as_deref() {
            Some("web") => ServiceKind::Web,
            Some("http") => ServiceKind::Http,
            _ => ServiceKind::Backend,
        };
        let web_mode = match (kind, raw.service.web_mode.as_deref()) {
            (ServiceKind::Web, Some("bff")) => Some(WebMode::Bff),
            (ServiceKind::Web, _) => Some(WebMode::Spa),
            _ => None,
        };

        // Effective listen port. Explicit `[service].port` wins; otherwise the
        // per-kind default — preserving pre-port-field output for web/backend.
        let port = raw.service.port.unwrap_or_else(|| match kind {
            ServiceKind::Web => web_mode.map(|m| m.container_port()).unwrap_or(8080),
            ServiceKind::Http => 8080,
            ServiceKind::Backend => 50051,
        });

        // Additional HTTP port for a gRPC backend that also serves HTTP
        // (`[service.http]`). None for http-primary (its primary port is already
        // HTTP) and for services with no secondary endpoint.
        let http_port = match kind {
            ServiceKind::Http => None,
            _ => raw.service.http.as_ref().map(|h| h.port),
        };

        // Health probe, auto-derived from the service's surface and always
        // present:
        //   * An HTTP surface (http/web kind, or a `[service.http]` port on a
        //     gRPC backend) → httpGet on that port.
        //   * A pure gRPC backend → a native Kubernetes `grpc:` probe
        //     (grpc.health.v1, served by the tonin runtime) on the gRPC port.
        //     httpGet on a gRPC port can never succeed, so we never emit it.
        // `[service.health]` overrides path/port for the HTTP case.
        let http_surface_port = match kind {
            ServiceKind::Http | ServiceKind::Web => Some(port),
            ServiceKind::Backend => http_port,
        };
        let health = Some(match http_surface_port {
            Some(hp) => {
                let declared = raw.service.health.as_ref();
                let secondary = raw.service.http.as_ref();
                HealthSpec {
                    grpc: false,
                    path: declared
                        .and_then(|h| h.path.clone())
                        .or_else(|| secondary.and_then(|h| h.health_path.clone()))
                        .unwrap_or_else(|| "/health".into()),
                    port: declared.and_then(|h| h.port).unwrap_or(hp),
                }
            }
            None => HealthSpec {
                grpc: true,
                path: String::new(),
                port,
            },
        });

        // Resolved language — used for both MCP and proxy derivation.
        let language = raw.service.language.as_deref().unwrap_or("rust");
        let is_rust = language == "rust";

        // McpMode: derived from language + kind. Explicit [deploy] mcp_sidecar=false
        // overrides to None (backward-compat escape hatch); true is a no-op.
        let mcp_mode = match mcp_sidecar_override {
            Some(false) => McpMode::None,
            _ => match kind {
                ServiceKind::Http | ServiceKind::Web => McpMode::None,
                ServiceKind::Backend if is_rust => McpMode::InProcess,
                ServiceKind::Backend => McpMode::Sidecar,
            },
        };

        // needs_outbound_proxy: true when language is non-Rust and this service
        // calls at least one downstream. [client] proxy=false is an escape hatch.
        let proxy_override = raw.client.as_ref().and_then(|c| c.proxy);
        let needs_outbound_proxy =
            !is_rust && !depends_on.is_empty() && proxy_override != Some(false);

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
                    proxy_override: c.proxy,
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

        // Reserved-name guard: `[env]` must not shadow env vars tonin already
        // emits (stateful literals like DATABASE_URL / REDIS_URL, or
        // secret-sourced keys). Duplicate container env names are
        // last-one-wins in Kubernetes — silently overriding a resolved URL is
        // exactly the drift tonin.toml exists to prevent, so hard-error.
        for (key, _) in &env_vars {
            if emitted_env.literals.iter().any(|(k, _)| k == key) {
                return Err(Error::ReservedEnvKey {
                    key: key.clone(),
                    origin: "stateful ([database]/[cache])".into(),
                });
            }
            if emitted_env.from_secret.iter().any(|k| k == key) {
                return Err(Error::ReservedEnvKey {
                    key: key.clone(),
                    origin: "secret-sourced ([secrets])".into(),
                });
            }
        }

        Ok(Plan {
            name: raw.service.name,
            version: raw.service.version,
            language: language.to_string(),
            kind,
            web_mode,
            port,
            http_port,
            health,
            namespace: deploy_namespace,
            mesh: deploy_mesh,
            replicas: deploy_replicas,
            max_replicas,
            mcp_mode,
            needs_outbound_proxy,
            expose: deploy_expose,
            cpu: raw.resources.cpu,
            memory: raw.resources.memory,
            image,
            image_registry,
            security,
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
            env_vars,
            selected_env: env.to_string(),
            path_rules: raw
                .path_rules
                .into_iter()
                .map(|r| PathRule {
                    pattern: r.pattern,
                    propagate_to_callers: r.propagate_to_callers,
                    layer: r.layer,
                    packages: r.packages,
                    build_order: r.build_order,
                })
                .collect(),
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

        let snapshot: Vec<(String, String, Vec<DependencyRef>)> = plans
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

/// Compute deployment wave groups from a slice of plans using topological sort.
///
/// Returns `Vec<Vec<&Plan>>` where each inner Vec is one wave. Plans in the
/// same wave have no dependency on each other and can be deployed in parallel.
/// Plans in wave N+1 all depend (directly or transitively) on plans in wave N.
///
/// Cycles are handled gracefully: if a cycle is detected, all cycle participants
/// land in wave 0 together (fail-safe, not fail-hard).
pub fn wave_groups(plans: &[Plan]) -> Vec<Vec<&Plan>> {
    use std::collections::HashMap;

    let name_to_idx: HashMap<&str, usize> = plans
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i))
        .collect();

    // in-degree counts how many *known* deps each plan has
    let mut in_degree = vec![0usize; plans.len()];
    for plan in plans {
        for dep in &plan.depends_on {
            if name_to_idx.contains_key(dep.name.as_str()) {
                in_degree[name_to_idx[plan.name.as_str()]] += 1;
            }
        }
    }

    // Kahn's BFS
    let mut queue: std::collections::VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter(|&(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();

    let mut wave_assignment = vec![0usize; plans.len()];
    let mut visited = 0usize;

    while !queue.is_empty() {
        // Drain current wave
        let current_wave: Vec<usize> = queue.drain(..).collect();
        visited += current_wave.len();

        for &idx in &current_wave {
            let plan = &plans[idx];
            let my_wave = wave_assignment[idx];

            for other in plans {
                if other.depends_on.iter().any(|d| d.name == plan.name)
                    && let Some(&oidx) = name_to_idx.get(other.name.as_str())
                {
                    in_degree[oidx] -= 1;
                    wave_assignment[oidx] = wave_assignment[oidx].max(my_wave + 1);
                    if in_degree[oidx] == 0 {
                        queue.push_back(oidx);
                    }
                }
            }
        }
    }

    // If cycle: any unvisited node goes to wave 0 (fail-safe)
    if visited < plans.len() {
        for i in 0..plans.len() {
            if in_degree[i] > 0 {
                wave_assignment[i] = 0;
            }
        }
    }

    // Group by wave
    let max_wave = wave_assignment.iter().copied().max().unwrap_or(0);
    let mut groups: Vec<Vec<&Plan>> = vec![vec![]; max_wave + 1];
    for (i, &wave) in wave_assignment.iter().enumerate() {
        groups[wave].push(&plans[i]);
    }
    groups.into_iter().filter(|g| !g.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    const BASE: &str = "\n[deploy]\nreplicas = 1\nnamespace = \"demo\"\n\n[resources]\ncpu = \"100m\"\nmemory = \"128Mi\"\n";

    /// Write `<service block> + BASE` to a temp tonin.toml and load it.
    fn load(service: &str) -> Plan {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tonin-plan-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tonin.toml");
        std::fs::write(&path, format!("{service}{BASE}")).unwrap();
        let plan = Plan::load_with_env(&path, "prod").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        plan
    }

    #[test]
    fn backend_defaults_unchanged() {
        let p = load("[service]\nname = \"svc\"\nversion = \"0.1.0\"");
        assert_eq!(p.kind, ServiceKind::Backend);
        assert_eq!(p.port, 50051);
        assert_eq!(p.http_port, None);
        // A pure gRPC backend gets an auto native grpc: probe on the gRPC port.
        let h = p.health.expect("backend gets an auto gRPC health probe");
        assert!(h.grpc, "gRPC service uses a grpc: probe");
        assert_eq!(h.port, 50051);
        assert_eq!(
            p.mcp_mode,
            McpMode::InProcess,
            "rust backend defaults to in-process MCP"
        );
        assert!(
            !p.needs_outbound_proxy,
            "rust service with no depends_on needs no proxy"
        );
    }

    #[test]
    fn backend_port_override() {
        let p = load("[service]\nname = \"svc\"\nversion = \"0.1.0\"\nport = 9090");
        assert_eq!(p.port, 9090);
    }

    #[test]
    fn http_kind_defaults_to_8080_with_default_probe_and_no_mcp() {
        let p = load("[service]\nname = \"svc\"\nversion = \"0.1.0\"\ntype = \"http\"");
        assert_eq!(p.kind, ServiceKind::Http);
        assert_eq!(p.port, 8080);
        let h = p.health.expect("http services get a default probe");
        assert_eq!(h.path, "/health");
        assert_eq!(h.port, 8080);
        assert_eq!(p.mcp_mode, McpMode::None, "http forces mcp_mode to None");
    }

    #[test]
    fn http_explicit_port_and_health_path() {
        let p = load(
            "[service]\nname = \"svc\"\nversion = \"0.1.0\"\ntype = \"http\"\nport = 7001\n[service.health]\npath = \"/healthz\"",
        );
        assert_eq!(p.port, 7001);
        let h = p.health.unwrap();
        assert_eq!(h.path, "/healthz");
        assert_eq!(h.port, 7001);
    }

    #[test]
    fn backend_with_http_exposes_both() {
        let p = load(
            "[service]\nname = \"svc\"\nversion = \"0.1.0\"\n[service.http]\nport = 8081\nhealth_path = \"/healthz\"",
        );
        assert_eq!(p.kind, ServiceKind::Backend);
        assert_eq!(p.port, 50051, "gRPC primary port preserved");
        assert_eq!(p.http_port, Some(8081));
        let h = p.health.unwrap();
        assert_eq!(h.path, "/healthz");
        assert_eq!(h.port, 8081, "probe targets the http port, not gRPC");
        assert_eq!(
            p.mcp_mode,
            McpMode::InProcess,
            "rust gRPC backend with http gets in-process MCP"
        );
    }

    // ---- depends_on per-env resolution ------------------------------------

    /// A complete tonin.toml with [service] + [resources]; tests append
    /// [deploy] and [depends_on]. Bodies use literal `{env}` (no format!),
    /// so they're concatenated rather than interpolated.
    const SVC: &str = "[service]\nname = \"svc\"\nversion = \"0.1.0\"\n[resources]\ncpu = \"100m\"\nmemory = \"128Mi\"\n";

    fn try_load_env(body: &str, env: &str) -> Result<Plan, Error> {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tonin-plan-dep-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tonin.toml");
        std::fs::write(&path, body).unwrap();
        let plan = Plan::load_with_env(&path, env);
        let _ = std::fs::remove_dir_all(&dir);
        plan
    }

    fn dep(plan: &Plan, name: &str) -> Option<String> {
        plan.depends_on
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.namespace.clone())
    }

    #[test]
    fn depends_on_literal_is_backward_compatible() {
        let body = [SVC, "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nusers-service = \"myapp-dev\"\n"].concat();
        let p = try_load_env(&body, "prod").unwrap();
        // No {env} → literal namespace, identical in every env (today's behaviour).
        assert_eq!(dep(&p, "users-service").as_deref(), Some("myapp-dev"));
    }

    #[test]
    fn depends_on_env_placeholder_resolves_per_env() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"myapp-{env}\"\n[depends_on]\nusers-service = \"myapp-{env}\"\n",
        ]
        .concat();
        let dev = try_load_env(&body, "dev").unwrap();
        assert_eq!(dev.namespace, "myapp-dev");
        assert_eq!(dep(&dev, "users-service").as_deref(), Some("myapp-dev"));
        let prod = try_load_env(&body, "prod").unwrap();
        assert_eq!(prod.namespace, "myapp-prod");
        assert_eq!(dep(&prod, "users-service").as_deref(), Some("myapp-prod"));
    }

    #[test]
    fn depends_on_table_per_env_override_wins() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\ninventory-service = { namespace = \"inventory-{env}\", prod = \"inventory-shared\" }\n",
        ]
        .concat();
        assert_eq!(
            dep(&try_load_env(&body, "dev").unwrap(), "inventory-service").as_deref(),
            Some("inventory-dev")
        );
        assert_eq!(
            dep(&try_load_env(&body, "prod").unwrap(), "inventory-service").as_deref(),
            Some("inventory-shared")
        );
    }

    #[test]
    fn depends_on_envs_whitelist_scopes_dependency() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\naudit = { namespace = \"security-{env}\", envs = [\"prod\"] }\n",
        ]
        .concat();
        assert!(
            dep(&try_load_env(&body, "dev").unwrap(), "audit").is_none(),
            "absent in dev"
        );
        assert_eq!(
            dep(&try_load_env(&body, "prod").unwrap(), "audit").as_deref(),
            Some("security-prod")
        );
    }

    #[test]
    fn depends_on_inherit_is_omitted_from_output() {
        let body = [SVC, "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nbilling = { namespace = \"@inherit\" }\n"].concat();
        assert!(dep(&try_load_env(&body, "prod").unwrap(), "billing").is_none());
    }

    #[test]
    fn depends_on_unresolved_placeholder_is_error() {
        let body = [SVC, "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nusers-service = \"myapp-{environment}\"\n"].concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(
            matches!(err, Error::UnresolvedNamespace { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn depends_on_missing_namespace_for_env_is_error() {
        // Only a dev override; prod has nothing to resolve to → hard error,
        // never a silent fallback.
        let body = [SVC, "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nusers-service = { dev = \"myapp-dev\" }\n"].concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(
            matches!(err, Error::InvalidDependency { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn depends_on_bare_string_defaults_port_50051() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nidentity = \"platform-{env}\"\n",
        ]
        .concat();
        let p = try_load_env(&body, "prod").unwrap();
        let d = p.depends_on.iter().find(|d| d.name == "identity").unwrap();
        assert_eq!(d.port, DEFAULT_DEPENDENCY_PORT, "shorthand keeps 50051");
    }

    #[test]
    fn depends_on_table_port_is_used() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nzradar-platform = { namespace = \"zradar-{env}\", port = 4317 }\ngateway = { namespace = \"edge-{env}\", port = 7001, prod = \"edge\" }\n",
        ]
        .concat();
        let p = try_load_env(&body, "prod").unwrap();
        let zr = p
            .depends_on
            .iter()
            .find(|d| d.name == "zradar-platform")
            .unwrap();
        assert_eq!(zr.namespace, "zradar-prod");
        assert_eq!(zr.port, 4317);
        let gw = p.depends_on.iter().find(|d| d.name == "gateway").unwrap();
        assert_eq!(gw.namespace, "edge", "per-env override still wins");
        assert_eq!(gw.port, 7001, "port applies alongside env overrides");
    }

    #[test]
    fn depends_on_table_without_port_defaults_to_50051() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nbilling = { namespace = \"billing-{env}\" }\n",
        ]
        .concat();
        let p = try_load_env(&body, "dev").unwrap();
        assert_eq!(p.depends_on[0].port, DEFAULT_DEPENDENCY_PORT);
    }

    #[test]
    fn depends_on_non_integer_port_is_error() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nidentity = { namespace = \"demo\", port = \"grpc\" }\n",
        ]
        .concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(
            matches!(err, Error::InvalidDependency { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn depends_on_out_of_range_port_is_error() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nidentity = { namespace = \"demo\", port = 70000 }\n",
        ]
        .concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(
            matches!(err, Error::InvalidDependency { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn depends_on_bad_type_is_error() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[depends_on]\nidentity = 123\n",
        ]
        .concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(
            matches!(err, Error::InvalidDependency { .. }),
            "got {err:?}"
        );
    }

    // ---- [env] plain env vars ---------------------------------------------

    fn env_var(plan: &Plan, key: &str) -> Option<String> {
        plan.env_vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn env_base_entries_resolve_with_env_placeholder() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[env]\nIDENTITY_GRPC_URL = \"http://identity.zvectorlabs-{env}.svc.cluster.local:50051\"\nLOG_FORMAT = \"json\"\n",
        ]
        .concat();
        let dev = try_load_env(&body, "dev").unwrap();
        assert_eq!(
            env_var(&dev, "IDENTITY_GRPC_URL").as_deref(),
            Some("http://identity.zvectorlabs-dev.svc.cluster.local:50051")
        );
        let prod = try_load_env(&body, "prod").unwrap();
        assert_eq!(
            env_var(&prod, "IDENTITY_GRPC_URL").as_deref(),
            Some("http://identity.zvectorlabs-prod.svc.cluster.local:50051")
        );
        assert_eq!(env_var(&prod, "LOG_FORMAT").as_deref(), Some("json"));
    }

    #[test]
    fn env_per_env_overlay_merges_over_base() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[env]\nLOG_FORMAT = \"json\"\nFEATURE_X = \"off\"\n[env.dev]\nLOG_FORMAT = \"pretty\"\nDEV_ONLY = \"1\"\n",
        ]
        .concat();
        let dev = try_load_env(&body, "dev").unwrap();
        assert_eq!(
            env_var(&dev, "LOG_FORMAT").as_deref(),
            Some("pretty"),
            "overlay wins"
        );
        assert_eq!(
            env_var(&dev, "FEATURE_X").as_deref(),
            Some("off"),
            "base keys survive the merge"
        );
        assert_eq!(env_var(&dev, "DEV_ONLY").as_deref(), Some("1"));
        let prod = try_load_env(&body, "prod").unwrap();
        assert_eq!(env_var(&prod, "LOG_FORMAT").as_deref(), Some("json"));
        assert!(
            env_var(&prod, "DEV_ONLY").is_none(),
            "dev-only stays in dev"
        );
    }

    #[test]
    fn env_non_string_value_is_error() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[env]\nPORT = 8080\n",
        ]
        .concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(matches!(err, Error::InvalidEnvVar { .. }), "got {err:?}");
    }

    #[test]
    fn env_non_string_value_in_overlay_is_error_even_when_inactive() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[env.dev]\nPORT = 8080\n",
        ]
        .concat();
        // Rendering prod, but the broken dev overlay must still fail loudly.
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(matches!(err, Error::InvalidEnvVar { .. }), "got {err:?}");
    }

    #[test]
    fn env_braces_other_than_env_pass_through() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[env]\nLOG_PATTERN = \"{level} {msg}\"\n",
        ]
        .concat();
        let p = try_load_env(&body, "prod").unwrap();
        assert_eq!(
            env_var(&p, "LOG_PATTERN").as_deref(),
            Some("{level} {msg}"),
            "env values may contain braces; only {{env}} is substituted"
        );
    }

    #[test]
    fn env_key_colliding_with_stateful_env_is_error() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[database]\nengine = \"postgres\"\n[env]\nDATABASE_URL = \"postgres://elsewhere/db\"\n",
        ]
        .concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(matches!(err, Error::ReservedEnvKey { .. }), "got {err:?}");
    }

    #[test]
    fn env_key_colliding_with_secret_key_is_error() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"demo\"\n[secrets]\nrequired = [\"JWT_SIGNING_KEY\"]\n[env]\nJWT_SIGNING_KEY = \"plaintext-oops\"\n",
        ]
        .concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(matches!(err, Error::ReservedEnvKey { .. }), "got {err:?}");
    }

    #[test]
    fn no_env_section_yields_empty_env_vars() {
        let p = load("[service]\nname = \"svc\"\nversion = \"0.1.0\"");
        assert!(p.env_vars.is_empty());
    }

    #[test]
    fn deploy_namespace_unresolved_placeholder_is_error() {
        let body = [
            SVC,
            "[deploy]\nreplicas = 1\nnamespace =\"myapp-{cluster}\"\n",
        ]
        .concat();
        let err = try_load_env(&body, "prod").unwrap_err();
        assert!(
            matches!(err, Error::UnresolvedNamespace { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn image_registry_from_toml_used_when_no_env_var() {
        let p = load(
            "[service]\nname = \"my-api\"\nversion = \"1.0.0\"\n\n[image]\nregistry = \"ghcr.io/myorg\"",
        );
        if std::env::var("TONIN_IMAGE_PREFIX").is_err() {
            assert_eq!(p.image, "ghcr.io/myorg/my-api:1.0.0");
            assert_eq!(p.image_registry.as_deref(), Some("ghcr.io/myorg"));
        }
    }

    #[test]
    fn no_image_section_yields_micro_fallback() {
        let p = load("[service]\nname = \"bare\"\nversion = \"0.1.0\"");
        if std::env::var("TONIN_IMAGE_PREFIX").is_err() {
            assert_eq!(p.image, "micro/bare:0.1.0");
            assert!(p.image_registry.is_none());
        }
    }

    #[test]
    fn security_spec_parsed_from_snake_case() {
        let p = load(
            r#"[service]
name    = "secure-svc"
version = "0.2.0"

[security.pod]
run_as_non_root = true
run_as_user     = 65532
run_as_group    = 65532
fs_group        = 65532

[security.container]
allow_privilege_escalation = false
read_only_root_filesystem  = true
drop_capabilities          = ["ALL"]
seccomp_profile_type       = "RuntimeDefault"
"#,
        );
        let sec = p.security.expect("security should be parsed");
        let pod = sec.pod.expect("pod section present");
        assert_eq!(
            pod.get("run_as_non_root").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            pod.get("run_as_user").and_then(|v| v.as_integer()),
            Some(65532)
        );
        let container = sec.container.expect("container section present");
        assert_eq!(
            container
                .get("allow_privilege_escalation")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            container
                .get("read_only_root_filesystem")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn security_spec_parsed_from_camel_case() {
        let p = load(
            r#"[service]
name    = "secure-svc"
version = "0.2.0"

[security.pod]
runAsNonRoot = true
runAsUser    = 65532

[security.container]
allowPrivilegeEscalation = false
"#,
        );
        let sec = p.security.expect("security should be parsed");
        let pod = sec.pod.expect("pod section present");
        assert_eq!(
            pod.get("runAsNonRoot").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            pod.get("runAsUser").and_then(|v| v.as_integer()),
            Some(65532)
        );
    }

    #[test]
    fn no_security_section_yields_none() {
        let p = load("[service]\nname = \"bare\"\nversion = \"0.1.0\"");
        assert!(p.security.is_none());
    }

    // ---- McpMode + needs_outbound_proxy derivation -----------------------

    const SVC_RUST: &str = "[service]\nname = \"svc\"\nversion = \"0.1.0\"\n[resources]\ncpu = \"50m\"\nmemory = \"64Mi\"\n";
    const SVC_PYTHON: &str = "[service]\nname = \"svc\"\nversion = \"0.1.0\"\nlanguage = \"python\"\n[resources]\ncpu = \"50m\"\nmemory = \"64Mi\"\n";
    const DEPLOY_WITH_DEP: &str =
        "[deploy]\nreplicas = 1\nnamespace = \"demo\"\n[depends_on]\npolicy = \"platform\"\n";
    const DEPLOY_NO_DEP: &str = "[deploy]\nreplicas = 1\nnamespace = \"demo\"\n";

    #[test]
    fn rust_with_depends_on_no_proxy_mcp_in_process() {
        let p = try_load_env(&format!("{SVC_RUST}{DEPLOY_WITH_DEP}"), "prod").unwrap();
        assert_eq!(p.mcp_mode, McpMode::InProcess);
        assert!(
            !p.needs_outbound_proxy,
            "rust always uses in-process coalescing"
        );
    }

    #[test]
    fn python_with_depends_on_gets_proxy_and_sidecar_mcp() {
        let p = try_load_env(&format!("{SVC_PYTHON}{DEPLOY_WITH_DEP}"), "prod").unwrap();
        assert_eq!(p.mcp_mode, McpMode::Sidecar);
        assert!(p.needs_outbound_proxy);
    }

    #[test]
    fn python_no_depends_on_no_proxy_but_sidecar_mcp() {
        let p = try_load_env(&format!("{SVC_PYTHON}{DEPLOY_NO_DEP}"), "prod").unwrap();
        assert_eq!(p.mcp_mode, McpMode::Sidecar);
        assert!(!p.needs_outbound_proxy, "no downstream = no proxy needed");
    }

    #[test]
    fn python_http_kind_no_mcp_but_proxy_when_has_depends_on() {
        let toml = format!(
            "[service]\nname = \"api\"\nversion = \"0.1.0\"\nlanguage = \"python\"\ntype = \"http\"\n\
             [resources]\ncpu = \"50m\"\nmemory = \"64Mi\"\n{DEPLOY_WITH_DEP}"
        );
        let p = try_load_env(&toml, "prod").unwrap();
        assert_eq!(p.mcp_mode, McpMode::None, "http kind forces MCP off");
        assert!(
            p.needs_outbound_proxy,
            "http python caller still needs proxy"
        );
    }

    #[test]
    fn proxy_escape_hatch_suppresses_auto_inject() {
        let toml = format!("{SVC_PYTHON}{DEPLOY_WITH_DEP}[client]\nproxy = false\n");
        let p = try_load_env(&toml, "prod").unwrap();
        assert!(
            !p.needs_outbound_proxy,
            "[client] proxy=false overrides auto-derive"
        );
        assert_eq!(
            p.mcp_mode,
            McpMode::Sidecar,
            "escape hatch only affects proxy, not MCP"
        );
    }

    #[test]
    fn mcp_sidecar_false_legacy_override_forces_mcp_none() {
        let toml = format!(
            "{SVC_RUST}[deploy]\nreplicas = 1\nnamespace = \"demo\"\nmcp_sidecar = false\n"
        );
        let p = try_load_env(&toml, "prod").unwrap();
        assert_eq!(
            p.mcp_mode,
            McpMode::None,
            "explicit mcp_sidecar=false still works as override"
        );
    }

    // ---- wave_groups --------------------------------------------------------

    fn make_test_plan(name: &str, depends_on: &[&str]) -> Plan {
        Plan {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            language: "rust".to_string(),
            kind: ServiceKind::Backend,
            web_mode: None,
            port: 50051,
            http_port: None,
            health: None,
            namespace: "demo".to_string(),
            mesh: Mesh::Cilium,
            replicas: 1,
            max_replicas: 1,
            mcp_mode: McpMode::InProcess,
            needs_outbound_proxy: false,
            expose: None,
            cpu: "100m".to_string(),
            memory: "128Mi".to_string(),
            image: "mock".to_string(),
            image_registry: None,
            security: None,
            depends_on: depends_on
                .iter()
                .map(|d| DependencyRef {
                    name: d.to_string(),
                    namespace: "demo".to_string(),
                    port: DEFAULT_DEPENDENCY_PORT,
                })
                .collect(),
            callers: Vec::new(),
            dir: std::path::PathBuf::from("."),
            database: None,
            named_databases: Vec::new(),
            cache: None,
            named_caches: Vec::new(),
            secrets: None,
            migrations: None,
            config: None,
            emitted_env: Default::default(),
            env_vars: Vec::new(),
            selected_env: "prod".to_string(),
            client: ClientSpec::default(),
            path_rules: Vec::new(),
        }
    }

    #[test]
    fn wave_groups_linear_3_node() {
        // users-service → inventory-service → orders-service
        // Expected: wave 0=[users], wave 1=[inventory], wave 2=[orders]
        let plans = vec![
            make_test_plan("users-service", &[]),
            make_test_plan("inventory-service", &["users-service"]),
            make_test_plan("orders-service", &["inventory-service"]),
        ];
        let groups = super::wave_groups(&plans);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0][0].name, "users-service");
        assert_eq!(groups[1][0].name, "inventory-service");
        assert_eq!(groups[2][0].name, "orders-service");
    }

    #[test]
    fn wave_groups_parallel_deps() {
        // users-service (wave 0)
        // inventory-service + payments-service both depend only on users-service → wave 1 parallel
        // orders-service depends on inventory-service → wave 2
        let plans = vec![
            make_test_plan("users-service", &[]),
            make_test_plan("inventory-service", &["users-service"]),
            make_test_plan("payments-service", &["users-service"]),
            make_test_plan("orders-service", &["inventory-service"]),
        ];
        let groups = super::wave_groups(&plans);
        let wave1_names: Vec<&str> = groups[1].iter().map(|p| p.name.as_str()).collect();
        assert!(
            wave1_names.contains(&"inventory-service"),
            "inventory must be wave 1"
        );
        assert!(
            wave1_names.contains(&"payments-service"),
            "payments must be wave 1 alongside inventory"
        );
        assert_eq!(groups[2][0].name, "orders-service");
    }

    #[test]
    fn wave_groups_no_deps_all_wave_0() {
        let plans = vec![
            make_test_plan("users-service", &[]),
            make_test_plan("inventory-service", &[]),
        ];
        let groups = super::wave_groups(&plans);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }
}
