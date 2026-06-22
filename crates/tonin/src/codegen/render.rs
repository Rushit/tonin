//! Render a `Plan` into a set of YAML files.
//!
//! Templates are embedded at compile time so the CLI is a single binary, but
//! can be overridden via:
//! - `TONIN_TEMPLATE_DIR` env var (local filesystem path to k8s/ subdir)
//! - `TONIN_TEMPLATE_VERSION` env var (GitHub release version, default: latest)
//! - Auto-download from <https://github.com/Rushit/tonin-templates/releases>
//!
//! Each output file is `RenderedFile { path, contents }` where `path` is
//! relative (e.g., `deployment.yaml`) — the caller decides where to write.

use include_dir::{Dir, include_dir};
use serde::Serialize;
use std::path::PathBuf;
use tera::{Context, Tera};

use super::plan::{Plan, ServiceRef};
use super::stateful::{CacheEngine, DatabaseEngine};

// Bundled templates. Pulled in at build time from crates/tonin/templates/.
static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates/k8s");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("template error: {0}")]
    Tera(#[from] tera::Error),
    #[error("template not found: {0}")]
    Missing(String),
}

#[derive(Debug, Clone)]
pub struct RenderedFile {
    pub path: String,
    pub contents: String,
}

/// Render one plan into its YAML files.
///
/// Output paths (relative):
/// - `deployment.yaml`, `service.yaml`, `hpa.yaml`
/// - `ingress.yaml` (if expose=ingress)
/// - `db-statefulset.yaml`, `db-service.yaml` (if `[database]` shared=false)
/// - `db-secret.yaml` (if `[database]` present at all — secret is needed
///   in either owned or shared mode so the password env var resolves)
/// - `cache-statefulset.yaml`, `cache-service.yaml` (if `[cache]` shared=false)
/// - mesh-specific files (cilium networkpolicy etc.)
pub fn render(plan: &Plan) -> Result<Vec<RenderedFile>, Error> {
    let mut tera = Tera::default();

    // Render the mesh annotation snippet first; the deployment template inlines it.
    let mesh_dir = format!("mesh/{}", plan.mesh.as_str());
    let mesh_pod_annotations = read_template(&format!("{mesh_dir}/pod-annotations.yaml.tmpl"))?;

    let mut ctx = base_context(plan)?;
    ctx.insert("mesh_pod_annotations", mesh_pod_annotations.trim_end());

    let mut out = Vec::new();

    // ---- Mesh-agnostic core resources ----
    let mut required: Vec<(&str, &str)> = vec![
        ("deployment.yaml.tmpl", "deployment.yaml"),
        ("service.yaml.tmpl", "service.yaml"),
        ("hpa.yaml.tmpl", "hpa.yaml"),
    ];
    if plan.expose.as_deref() == Some("ingress") {
        required.push(("ingress.yaml.tmpl", "ingress.yaml"));
    }

    // ---- Stateful: DB / cache / secret ----
    // Owned DB → render its StatefulSet + Service.
    // Any DB → render the credentials Secret (envFrom in the deployment
    // needs the Secret to exist whether the DB pod is owned or shared).
    let has_database = plan
        .database
        .as_ref()
        .is_some_and(|d| !matches!(d.engine, DatabaseEngine::None));
    let db_owned = has_database && plan.database.as_ref().is_some_and(|d| !d.shared);
    if has_database {
        required.push(("db-secret.yaml.tmpl", "db-secret.yaml"));
    }
    if db_owned {
        required.push(("db-statefulset.yaml.tmpl", "db-statefulset.yaml"));
        required.push(("db-service.yaml.tmpl", "db-service.yaml"));
    }
    // App secrets (JWT keys, API tokens, etc.) declared in [secrets] required.
    // Renders a separate `secrets.yaml` with placeholder values — distinct
    // from db-secret.yaml which holds only DATABASE_PASSWORD.
    let has_app_secrets_manifest = plan
        .secrets
        .as_ref()
        .is_some_and(|s| !s.required.is_empty());
    if has_app_secrets_manifest {
        required.push(("secrets.yaml.tmpl", "secrets.yaml"));
    }

    let cache_owned = plan
        .cache
        .as_ref()
        .is_some_and(|c| !c.shared && !matches!(c.engine, CacheEngine::None));
    if cache_owned {
        required.push(("cache-statefulset.yaml.tmpl", "cache-statefulset.yaml"));
        required.push(("cache-service.yaml.tmpl", "cache-service.yaml"));
    }

    for (tmpl_name, out_name) in required {
        let src = read_template(tmpl_name)?;
        tera.add_raw_template(tmpl_name, &src)?;
        let rendered = tera.render(tmpl_name, &ctx)?;
        out.push(RenderedFile {
            path: out_name.into(),
            contents: rendered,
        });
    }

    // ---- Mesh-specific resources ----
    if let Some(dir) = TEMPLATES.get_dir(&mesh_dir) {
        for entry in dir.files() {
            let name = entry.path().file_name().unwrap().to_string_lossy();
            if name == "pod-annotations.yaml.tmpl" || !name.ends_with(".yaml.tmpl") {
                continue;
            }
            let tmpl_key = format!("{mesh_dir}/{name}");
            let src = std::str::from_utf8(entry.contents()).expect("template is utf8");
            tera.add_raw_template(&tmpl_key, src)?;
            let rendered = tera.render(&tmpl_key, &ctx)?;
            let out_name = name.trim_end_matches(".tmpl").to_string();
            out.push(RenderedFile {
                path: out_name,
                contents: rendered,
            });
        }
    }

    Ok(out)
}

fn read_template(rel: &str) -> Result<String, Error> {
    // Check TONIN_TEMPLATE_DIR environment variable first (for local development).
    // Set to the k8s/ subdirectory, e.g. `TONIN_TEMPLATE_DIR=/path/to/tonin-templates/k8s`
    if let Ok(template_dir) = std::env::var("TONIN_TEMPLATE_DIR") {
        let path = PathBuf::from(&template_dir).join(rel);
        if path.exists()
            && let Ok(contents) = std::fs::read_to_string(&path)
        {
            return Ok(contents);
        }
    }

    // Fall back to embedded templates from compile time.
    let f = TEMPLATES
        .get_file(rel)
        .ok_or_else(|| Error::Missing(rel.into()))?;
    Ok(std::str::from_utf8(f.contents())
        .expect("template is utf8")
        .to_string())
}

#[derive(Serialize)]
struct ServiceRefCtx {
    name: String,
    namespace: String,
}

impl From<&ServiceRef> for ServiceRefCtx {
    fn from(s: &ServiceRef) -> Self {
        Self {
            name: s.name.clone(),
            namespace: s.namespace.clone(),
        }
    }
}

fn base_context(plan: &Plan) -> Result<Context, Error> {
    let mut ctx = Context::new();
    ctx.insert("name", &plan.name);
    ctx.insert("version", &plan.version);
    ctx.insert("namespace", &plan.namespace);
    ctx.insert("mesh", plan.mesh.as_str());
    ctx.insert("replicas", &plan.replicas);
    ctx.insert("max_replicas", &plan.max_replicas);
    ctx.insert("mcp_sidecar", &plan.mcp_sidecar);
    ctx.insert("cpu", &plan.cpu);
    ctx.insert("memory", &plan.memory);
    ctx.insert("image", &plan.image);

    // Web / http / backend toggles in templates.
    ctx.insert("kind", plan.kind.as_str());
    ctx.insert("is_web", &plan.kind.is_web());
    ctx.insert("is_http", &plan.kind.is_http());
    ctx.insert("port", &plan.port);
    ctx.insert("web_mode", &plan.web_mode.map(|m| m.as_str()).unwrap_or(""));
    ctx.insert("ingress", &(plan.expose.as_deref() == Some("ingress")));

    // Additional HTTP port (a gRPC backend that also serves HTTP).
    ctx.insert("has_http_port", &plan.http_port.is_some());
    if let Some(p) = plan.http_port {
        ctx.insert("http_port", &p);
    }

    // HTTP health probe (httpGet). Absent for web/backend unless declared, so
    // their manifests stay byte-identical.
    ctx.insert("has_health", &plan.health.is_some());
    if let Some(h) = &plan.health {
        ctx.insert("health_path", &h.path);
        ctx.insert("health_port", &h.port);
    }

    let deps: Vec<ServiceRefCtx> = plan.depends_on.iter().map(Into::into).collect();
    let callers: Vec<ServiceRefCtx> = plan.callers.iter().map(Into::into).collect();
    ctx.insert("depends_on", &deps);
    ctx.insert("callers", &callers);

    // ---- Stateful fields ----
    let has_database = plan
        .database
        .as_ref()
        .is_some_and(|d| !matches!(d.engine, DatabaseEngine::None));
    ctx.insert("has_database", &has_database);
    ctx.insert("has_db_secret", &has_database);
    if let Some(db) = &plan.database {
        ctx.insert("db_engine", db.engine.as_str());
        ctx.insert("db_name", &db.name);
        ctx.insert("db_namespace", &db.namespace);
        ctx.insert("db_port", &db.port());
        ctx.insert("db_size", &db.size);
        ctx.insert("db_image", &db.image());
        ctx.insert("db_shared", &db.shared);
    } else {
        ctx.insert("db_engine", "");
        ctx.insert("db_name", "");
        ctx.insert("db_namespace", "");
        ctx.insert("db_port", &0_u32);
    }

    let has_cache = plan
        .cache
        .as_ref()
        .is_some_and(|c| !matches!(c.engine, CacheEngine::None));
    ctx.insert("has_cache", &has_cache);
    if let Some(c) = &plan.cache {
        ctx.insert("cache_engine", c.engine.as_str());
        ctx.insert("cache_name", &c.name);
        ctx.insert("cache_namespace", &c.namespace);
        ctx.insert("cache_port", &c.port());
        ctx.insert("cache_size", &c.size);
        ctx.insert("cache_shared", &c.shared);
    } else {
        ctx.insert("cache_engine", "");
        ctx.insert("cache_name", "");
        ctx.insert("cache_namespace", "");
        ctx.insert("cache_port", &0_u32);
    }

    // Literal envs the deployment template iterates over (DATABASE_URL,
    // REDIS_URL, etc.). Tera receives them as a list of (key, value) tuples.
    let literals: Vec<(String, String)> = plan.emitted_env.literals.to_vec();
    ctx.insert("stateful_env_literals", &literals);

    // App secret env vars declared in `[secrets] required` (e.g. JWT_SIGNING_KEY).
    // Rendered as `valueFrom: secretKeyRef` pointing at `<name>-secrets`.
    // Intentionally separate from emitted_env.from_secret which also contains
    // DATABASE_PASSWORD — that key is already covered by the db-credentials
    // envFrom block and must not appear here to avoid duplicate env var errors.
    let app_secret_keys: Vec<String> = plan
        .secrets
        .as_ref()
        .map(|s| s.required.clone())
        .unwrap_or_default();
    let has_app_secrets = !app_secret_keys.is_empty();
    ctx.insert("stateful_env_from_secret", &app_secret_keys);
    ctx.insert("has_app_secrets", &has_app_secrets);

    // Init container fields for migrations.
    let has_migrations_init = plan
        .migrations
        .as_ref()
        .is_some_and(|m| matches!(m.run_on, super::stateful::MigrationRunOn::InitContainer));
    ctx.insert("has_migrations_init", &has_migrations_init);
    if let Some(m) = &plan.migrations {
        ctx.insert("migrations_command", &m.command);
        ctx.insert("migrations_dir", &m.dir);
    } else {
        let empty: Vec<String> = Vec::new();
        ctx.insert("migrations_command", &empty);
        ctx.insert("migrations_dir", "");
    }

    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Render `<service block>` (plus a minimal cilium deploy) into a map of
    /// output filename → contents.
    fn render_files(service: &str) -> HashMap<String, String> {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tonin-render-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!(
            "{service}\n[deploy]\nreplicas = 1\nnamespace = \"demo\"\nmesh = \"cilium\"\n[resources]\ncpu = \"100m\"\nmemory = \"128Mi\"\n"
        );
        let path = dir.join("tonin.toml");
        std::fs::write(&path, body).unwrap();
        let plan = Plan::load_with_env(&path, "prod").unwrap();
        let files = render(&plan).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        files.into_iter().map(|f| (f.path, f.contents)).collect()
    }

    #[test]
    fn backend_renders_grpc_service_without_probe() {
        let f = render_files("[service]\nname = \"svc\"\nversion = \"0.1.0\"");
        let svc = &f["service.yaml"];
        assert!(svc.contains("name: grpc"));
        assert!(svc.contains("port: 50051"));
        let dep = &f["deployment.yaml"];
        assert!(dep.contains("name: grpc"));
        assert!(!dep.contains("livenessProbe"), "backend gets no http probe");
    }

    #[test]
    fn http_renders_http_service_with_probe_and_no_mcp() {
        let f = render_files(
            "[service]\nname = \"svc\"\nversion = \"0.1.0\"\ntype = \"http\"\nport = 7001",
        );
        let svc = &f["service.yaml"];
        assert!(svc.contains("name: http"));
        assert!(svc.contains("port: 7001"));
        assert!(!svc.contains("grpc"));
        assert!(
            !svc.contains("name: mcp"),
            "http forces the mcp sidecar off"
        );
        let dep = &f["deployment.yaml"];
        assert!(dep.contains("name: http"));
        assert!(dep.contains("livenessProbe"));
        assert!(dep.contains("readinessProbe"));
        assert!(dep.contains("path: /health"));
        assert!(!dep.contains("name: mcp"));
    }

    #[test]
    fn backend_with_http_renders_both_ports() {
        let f = render_files(
            "[service]\nname = \"svc\"\nversion = \"0.1.0\"\n[service.http]\nport = 8081",
        );
        let svc = &f["service.yaml"];
        assert!(svc.contains("name: grpc"));
        assert!(svc.contains("name: http"));
        assert!(svc.contains("port: 8081"));
        let dep = &f["deployment.yaml"];
        assert!(dep.contains("containerPort: 8081"));
        assert!(dep.contains("livenessProbe"));
        assert!(dep.contains("port: 8081"), "probe targets the http port");
    }

    // ---- per-env depends_on → CiliumNetworkPolicy egress -------------------

    /// Render a full tonin.toml `body` at `env` into filename → contents.
    fn render_env(body: &str, env: &str) -> HashMap<String, String> {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tonin-render-env-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tonin.toml");
        std::fs::write(&path, body).unwrap();
        let plan = Plan::load_with_env(&path, env).unwrap();
        let files = render(&plan).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        files.into_iter().map(|f| (f.path, f.contents)).collect()
    }

    // Generic service exercising every depends_on form: shorthand `{env}`,
    // a table with a per-env override, a prod-only dependency, and `@inherit`.
    const ORDERS: &str = "[service]
name = \"orders\"
version = \"0.1.0\"
type = \"http\"
port = 7001
[deploy]
replicas = 1
namespace = \"orders-{env}\"
mesh = \"cilium\"
[resources]
cpu = \"100m\"
memory = \"128Mi\"
[depends_on]
identity = \"platform-{env}\"
billing = { namespace = \"billing-{env}\", prod = \"billing-shared\" }
audit = { namespace = \"security-{env}\", envs = [\"prod\"] }
external = { namespace = \"@inherit\" }
";

    #[test]
    fn depends_on_table_renders_prod_egress() {
        let f = render_env(ORDERS, "prod");
        let np = &f["networkpolicy.yaml"];
        // Policy is scoped to the service's own per-env namespace.
        assert!(np.contains("service.identity: orders.orders-prod"), "{np}");
        // `{env}` egress target resolves to prod.
        assert!(
            np.contains("service.identity: identity.platform-prod"),
            "{np}"
        );
        // Per-env override wins over the default namespace.
        assert!(
            np.contains("service.identity: billing.billing-shared"),
            "{np}"
        );
        // Prod-only dependency is present in prod.
        assert!(np.contains("service.identity: audit.security-prod"), "{np}");
        // `@inherit` is omitted from the rendered policy.
        assert!(
            !np.contains("service.identity: external"),
            "@inherit must not render: {np}"
        );
    }

    #[test]
    fn depends_on_table_renders_dev_egress() {
        let f = render_env(ORDERS, "dev");
        let np = &f["networkpolicy.yaml"];
        assert!(np.contains("service.identity: orders.orders-dev"), "{np}");
        assert!(
            np.contains("service.identity: identity.platform-dev"),
            "{np}"
        );
        // dev uses the default namespace, not the prod override.
        assert!(np.contains("service.identity: billing.billing-dev"), "{np}");
        // audit is prod-only → absent in dev.
        assert!(
            !np.contains("service.identity: audit"),
            "audit is prod-only: {np}"
        );
    }
}
