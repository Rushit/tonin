//! `tonin helm generate` — render a Helm chart from `tonin.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};
use tera::Tera;
use tonin_plugin::{McpMode, MigrationRunOn, Plan, SecuritySection, ServiceKind};

static TEMPLATES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates/helm");

/// Implemented by each optional `tonin.toml` section that contributes keys to the Tera context.
/// Adding a new section means a new `impl` here — `build_context` stays stable.
trait HelmContributor {
    fn contribute(&self, ctx: &mut tera::Context);
}

impl HelmContributor for Option<&SecuritySection> {
    fn contribute(&self, ctx: &mut tera::Context) {
        match self {
            None => {
                ctx.insert("has_pod_security", &false);
                ctx.insert("has_container_security", &false);
                ctx.insert("pod_security_context_yaml", &"");
                ctx.insert("container_security_context_yaml", &"");
            }
            Some(sec) => {
                match &sec.pod {
                    None => {
                        ctx.insert("has_pod_security", &false);
                        ctx.insert("pod_security_context_yaml", &"");
                    }
                    Some(val) => {
                        ctx.insert("has_pod_security", &true);
                        ctx.insert("pod_security_context_yaml", &to_indented_yaml(val));
                    }
                }
                match &sec.container {
                    None => {
                        ctx.insert("has_container_security", &false);
                        ctx.insert("container_security_context_yaml", &"");
                    }
                    Some(val) => {
                        ctx.insert("has_container_security", &true);
                        ctx.insert("container_security_context_yaml", &to_indented_yaml(val));
                    }
                }
            }
        }
    }
}

/// Convert a TOML value to a 2-space-indented YAML string for embedding in `values.yaml`.
/// Table keys are converted from `snake_case` to `camelCase`; already-camelCase keys pass through.
fn to_indented_yaml(val: &toml::Value) -> String {
    let converted = normalize_keys(val.clone());
    let raw = serde_yaml::to_string(&converted).unwrap_or_default();
    let content = raw.strip_prefix("---\n").unwrap_or(&raw);
    content
        .lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recursively convert `snake_case` keys to `camelCase`. Keys without underscores pass through
/// unchanged so callers can also write `camelCase` directly in `tonin.toml`.
fn normalize_keys(val: toml::Value) -> toml::Value {
    match val {
        toml::Value::Table(tbl) => {
            let converted = tbl
                .into_iter()
                .map(|(k, v)| (snake_to_camel(&k), normalize_keys(v)))
                .collect();
            toml::Value::Table(converted)
        }
        toml::Value::Array(arr) => {
            toml::Value::Array(arr.into_iter().map(normalize_keys).collect())
        }
        other => other,
    }
}

fn snake_to_camel(key: &str) -> String {
    if !key.contains('_') {
        return key.to_string();
    }
    let mut parts = key.split('_');
    let first = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest: String = parts
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect();
    first + &rest
}

/// Default environments rendered unless `--envs` overrides.
const DEFAULT_ENVS: &[&str] = &["dev", "staging", "prod"];

#[derive(clap::Args)]
pub struct GenerateArgs {
    /// Path to the service directory containing `tonin.toml`.
    /// Defaults to `$TONIN_SERVICE_DIR` if set, otherwise the current directory.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// Where to write the generated chart.
    /// Defaults to `<path>/chart`.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Environments to generate `values-<env>.yaml` for.
    /// Defaults to: dev, staging, prod.
    #[arg(long = "env", value_name = "ENV")]
    pub envs: Vec<String>,
}

pub fn run(args: GenerateArgs) -> Result<()> {
    let service_dir = resolve_service_dir(args.path.as_deref())?;
    let toml_path = service_dir.join("tonin.toml");

    let base_plan =
        Plan::load(&toml_path).with_context(|| format!("loading {}", toml_path.display()))?;

    let chart_dir = args.out.unwrap_or_else(|| service_dir.join("chart"));
    let templates_dir = chart_dir.join("templates");
    std::fs::create_dir_all(&templates_dir)
        .with_context(|| format!("creating {}", templates_dir.display()))?;

    // --- Chart.yaml --------------------------------------------------------
    let ctx = build_context(&base_plan);
    render_tmpl("Chart.yaml.tmpl", &ctx, &chart_dir.join("Chart.yaml"))?;

    // --- values.yaml (base) ------------------------------------------------
    render_tmpl("values.yaml.tmpl", &ctx, &chart_dir.join("values.yaml"))?;

    // --- values-<env>.yaml -------------------------------------------------
    let env_names: Vec<String> = if args.envs.is_empty() {
        DEFAULT_ENVS.iter().map(|s| s.to_string()).collect()
    } else {
        args.envs
    };

    for env_name in &env_names {
        let env_plan = Plan::load_with_env(&toml_path, env_name)
            .with_context(|| format!("loading plan for env '{env_name}'"))?;
        let mut env_ctx = build_context(&env_plan);
        env_ctx.insert("env_name", env_name);
        render_tmpl(
            "values-env.yaml.tmpl",
            &env_ctx,
            &chart_dir.join(format!("values-{env_name}.yaml")),
        )?;
    }

    // --- templates/ (static Go template files — copy verbatim) -------------
    copy_static_templates(&templates_dir)?;

    println!("chart generated at {}", chart_dir.display());
    Ok(())
}

fn build_context(plan: &Plan) -> tera::Context {
    let mut ctx = tera::Context::new();
    ctx.insert("name", &plan.name);
    ctx.insert("version", &plan.version);
    ctx.insert("image_repository", &image_repository(&plan.image));
    ctx.insert("replicas", &plan.replicas);
    ctx.insert("max_replicas", &plan.max_replicas);
    ctx.insert("has_autoscale", &(plan.max_replicas > plan.replicas));
    ctx.insert("mcp_enabled", &(plan.mcp_mode != McpMode::None));
    ctx.insert("mcp_mode", &plan.mcp_mode.as_str());
    ctx.insert("needs_proxy", &plan.needs_outbound_proxy);
    ctx.insert("proxy_port", &6565_u16);
    ctx.insert("cpu", &plan.cpu);
    ctx.insert("memory", &plan.memory);
    ctx.insert("namespace", &plan.namespace);
    ctx.insert("mesh", plan.mesh.as_str());
    ctx.insert("is_cilium", &(plan.mesh.as_str() == "cilium"));
    ctx.insert("expose", &plan.expose);

    // Mesh-derived pod annotations. Generated (not hand-edited) so they survive
    // `tonin helm generate`. Cilium gets transparent (WireGuard) encryption.
    let mut pod_annotations: Vec<(String, String)> = Vec::new();
    if plan.mesh.as_str() == "cilium" {
        pod_annotations.push(("io.cilium/encryption".into(), "enabled".into()));
    }
    ctx.insert("pod_annotations", &pod_annotations);

    // Service protocol. `backend` → gRPC; `http` → HTTP; `web` → frontend.
    // A gRPC backend can ALSO expose an HTTP port via `plan.http_port`.
    let is_web = matches!(plan.kind, ServiceKind::Web);
    let is_http = matches!(plan.kind, ServiceKind::Http);
    let is_rust = plan.language == "rust";
    ctx.insert("is_web", &is_web);
    ctx.insert("is_http", &is_http);
    ctx.insert("is_rust", &is_rust);
    ctx.insert("port", &plan.port);
    ctx.insert("port_name", if is_web || is_http { "http" } else { "grpc" });
    // Secondary HTTP port (0 when absent), so the chart can render it alongside gRPC.
    ctx.insert("http_port", &plan.http_port.unwrap_or(0));

    // Health probe. Always insert the keys (Tera needs them defined); the chart
    // renders a probe when `health_enabled` is true. `health_grpc` selects a
    // native `grpc:` probe (gRPC services) vs `httpGet` (http/web/backend-http).
    ctx.insert("health_enabled", &plan.health.is_some());
    match &plan.health {
        Some(h) => {
            ctx.insert("health_path", &h.path);
            ctx.insert("health_port", &h.port);
            ctx.insert("health_grpc", &h.grpc);
        }
        None => {
            ctx.insert("health_path", "/health");
            ctx.insert("health_port", &plan.port);
            ctx.insert("health_grpc", &false);
        }
    }

    // ---- Database. Always insert every key (with defaults) so the Tera
    //      templates never hit an undefined variable when [database] is absent.
    let db_enabled = plan
        .database
        .as_ref()
        .is_some_and(|d| d.engine.as_str() != "none");
    ctx.insert("db_enabled", &db_enabled);
    match &plan.database {
        Some(db) => {
            ctx.insert("db_engine", db.engine.as_str());
            ctx.insert("db_shared", &db.shared);
            ctx.insert("db_name", &db.name);
            ctx.insert("db_namespace", &db.namespace);
            ctx.insert("db_port", &db.port());
            ctx.insert("db_image", &db.image());
            ctx.insert("db_size", &db.size);
        }
        None => {
            ctx.insert("db_engine", "none");
            ctx.insert("db_shared", &false);
            ctx.insert("db_name", "");
            ctx.insert("db_namespace", "");
            ctx.insert("db_port", &5432_u32);
            ctx.insert("db_image", "");
            ctx.insert("db_size", "2Gi");
        }
    }

    // ---- Cache. Same always-insert discipline.
    let cache_enabled = plan
        .cache
        .as_ref()
        .is_some_and(|c| c.engine.as_str() != "none");
    ctx.insert("cache_enabled", &cache_enabled);
    match &plan.cache {
        Some(c) => {
            ctx.insert("cache_engine", c.engine.as_str());
            ctx.insert("cache_shared", &c.shared);
            ctx.insert("cache_name", &c.name);
            ctx.insert("cache_namespace", &c.namespace);
            ctx.insert("cache_port", &c.port());
            ctx.insert("cache_size", &c.size);
        }
        None => {
            ctx.insert("cache_engine", "none");
            ctx.insert("cache_shared", &false);
            ctx.insert("cache_name", "");
            ctx.insert("cache_namespace", "");
            ctx.insert("cache_port", &6379_u32);
            ctx.insert("cache_size", "1Gi");
        }
    }

    // ---- Stateful env + secret keys. `emitted_env` already resolves
    //      DATABASE_URL / REDIS_URL (literals) and the from-secret key list
    //      (DATABASE_PASSWORD + declared [secrets].required) for this env.
    ctx.insert("stateful_env_literals", &plan.emitted_env.literals);
    ctx.insert("secret_keys", &plan.emitted_env.from_secret);

    // ---- Migrations. `enabled` when tonin.toml asks for a managed migration
    //      step (run_on = init-container). The chart renders either an
    //      initContainer (default, dev / owned DB) or a pre-upgrade hook Job
    //      (prod / managed DB) selected by `migrations.mode` at deploy time.
    let migrations_enabled = plan
        .migrations
        .as_ref()
        .is_some_and(|m| matches!(m.run_on, MigrationRunOn::InitContainer));
    ctx.insert("migrations_enabled", &migrations_enabled);
    let migrations_command: Vec<String> = plan
        .migrations
        .as_ref()
        .map(|m| m.command.clone())
        .unwrap_or_default();
    ctx.insert("migrations_command", &migrations_command);

    // ---- CiliumNetworkPolicy allowlist (callers) + egress (depends_on).
    ctx.insert("callers", &plan.callers);
    ctx.insert("depends_on", &plan.depends_on);

    ctx.insert("env_name", &plan.selected_env);

    plan.security.as_ref().contribute(&mut ctx);

    ctx
}

fn render_tmpl(tmpl_name: &str, ctx: &tera::Context, out_path: &Path) -> Result<()> {
    let tmpl_file = TEMPLATES
        .get_file(tmpl_name)
        .with_context(|| format!("missing embedded template '{tmpl_name}'"))?;
    let tmpl_src = tmpl_file
        .contents_utf8()
        .with_context(|| format!("template '{tmpl_name}' is not valid UTF-8"))?;

    let mut tera = Tera::default();
    tera.add_raw_template(tmpl_name, tmpl_src)
        .with_context(|| format!("parsing template '{tmpl_name}'"))?;

    let rendered = tera
        .render(tmpl_name, ctx)
        .with_context(|| format!("rendering template '{tmpl_name}'"))?;

    // Atomic write: temp file → rename
    let parent = out_path.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(tmp.path(), &rendered)?;
    tmp.persist(out_path)
        .with_context(|| format!("writing {}", out_path.display()))?;
    Ok(())
}

/// Copy every file under `templates/chart-templates/` into `out_dir` verbatim.
/// These are Go template files — Tera must not process them.
fn copy_static_templates(out_dir: &Path) -> Result<()> {
    let subdir = TEMPLATES
        .get_dir("chart-templates")
        .context("missing embedded 'chart-templates/' directory")?;
    for file in subdir.files() {
        let file_name = file
            .path()
            .file_name()
            .context("embedded file has no file name")?;
        let dest = out_dir.join(file_name);
        let tmp = tempfile::NamedTempFile::new_in(out_dir)?;
        std::fs::write(tmp.path(), file.contents())?;
        tmp.persist(&dest)
            .with_context(|| format!("writing {}", dest.display()))?;
    }
    Ok(())
}

/// Strip the `:tag` suffix from a full image string to get the repository.
fn image_repository(image: &str) -> &str {
    image.split(':').next().unwrap_or(image)
}

pub fn resolve_service_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(dir) = std::env::var("TONIN_SERVICE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    std::env::current_dir().context("cannot determine current directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `tonin.toml` exercising every stateful surface: owned DB + cache in
    /// prod, a shared DB in dev, app secrets, callers (with a dev overlay),
    /// migrations, and the cilium mesh.
    const RICH_TOML: &str = r#"
schema = "v1"

[service]
name    = "identity"
version = "1.2.3"

[deploy]
replicas    = 2
mesh        = "cilium"
namespace   = "myapp"
mcp_sidecar = true

[deploy.dev]
replicas  = 1
namespace = "myapp-dev"

[resources]
cpu    = "100m"
memory = "128Mi"

[database]
engine = "postgres"

[database.dev]
shared = true
url    = "postgresql://u:p@postgres.shared-dev.svc.cluster.local:5432/identity_dev"

[cache]
engine = "redis"

[cache.dev]
shared = true
url    = "redis://redis.shared-dev.svc.cluster.local:6379"

[secrets]
required = ["JWT_SIGNING_KEY"]

[migrations]
tool   = "sqlx"
dir    = "migrations/"
run_on = "init-container"

[callers]
orders-service = "myapp"

[callers.dev]
orders-service = "myapp-dev"
"#;

    fn write_service(dir: &Path, toml: &str) {
        std::fs::write(dir.join("tonin.toml"), toml).unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn generates_full_chart_tree() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");

        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["dev".into(), "prod".into()],
        })
        .unwrap();

        // Static Go templates for every resource are copied in.
        for f in [
            "deployment.yaml",
            "service.yaml",
            "hpa.yaml",
            "db-statefulset.yaml",
            "db-service.yaml",
            "cache-statefulset.yaml",
            "cache-service.yaml",
            "secret.yaml",
            "networkpolicy.yaml",
        ] {
            assert!(
                out.join("templates").join(f).exists(),
                "missing template {f}"
            );
        }
        assert!(out.join("values.yaml").exists());
        assert!(out.join("values-dev.yaml").exists());
        assert!(out.join("values-prod.yaml").exists());
    }

    #[test]
    fn prod_values_own_the_database_and_carry_db_password_secret() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let prod = read(&out.join("values-prod.yaml"));
        // prod has no [database.prod] overlay -> owned (shared:false).
        assert!(prod.contains("shared: false"), "prod db should be owned");
        assert!(prod.contains("name: \"identity-db\""));
        // Owned DB means DATABASE_PASSWORD is a required secret key.
        assert!(prod.contains("DATABASE_PASSWORD"));
        assert!(prod.contains("JWT_SIGNING_KEY"));
        // Cilium policy allowlist resolves the base caller namespace.
        assert!(prod.contains("name: orders-service"));
        assert!(prod.contains("namespace: myapp"));
    }

    #[test]
    fn dev_values_use_shared_instances_without_db_password() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["dev".into()],
        })
        .unwrap();

        let dev = read(&out.join("values-dev.yaml"));
        assert!(dev.contains("shared: true"), "dev db should be shared");
        // Shared DB (url override) emits no DATABASE_PASSWORD secret key.
        assert!(
            !dev.contains("DATABASE_PASSWORD"),
            "shared dev db must not require DATABASE_PASSWORD"
        );
        // The shared URL is injected verbatim as stateful env.
        assert!(dev.contains("postgres.shared-dev.svc.cluster.local"));
        // Caller dev overlay applies.
        assert!(dev.contains("namespace: myapp-dev"));
    }

    #[test]
    fn migrations_default_to_init_container_mode_and_ship_job_template() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("enabled: true")); // [migrations] present
        assert!(values.contains("mode: init-container")); // default mode
        assert!(values.contains("env: {}")); // migration-only env hatch
        // Both the initContainer (deployment) and the Job hook template ship;
        // mode selects which renders at deploy time.
        assert!(out.join("templates").join("migration-job.yaml").exists());
        let job = read(&out.join("templates").join("migration-job.yaml"));
        assert!(job.contains("helm.sh/hook\": pre-install,pre-upgrade"));
        assert!(job.contains(r#"eq .Values.migrations.mode "job""#));
    }

    #[test]
    fn names_resources_by_chart_not_release_and_orders_secret_env_first() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        // service.name resolves to the chart name (fixed service identity), not
        // the deploy-time release name — so binary paths + mesh labels are stable.
        let helpers = read(&out.join("templates").join("_helpers.tpl"));
        assert!(helpers.contains(".Chart.Name"));
        assert!(!helpers.contains(".Release.Name"));

        // Secret-sourced env (DATABASE_PASSWORD) must precede the statefulEnv
        // literals (DATABASE_URL) so k8s can expand $(DATABASE_PASSWORD).
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        let secrets_at = deployment.find(".Values.secrets.keys").unwrap();
        let stateful_at = deployment.find(".Values.statefulEnv").unwrap();
        assert!(
            secrets_at < stateful_at,
            "secrets.keys must be rendered before statefulEnv"
        );
    }

    #[test]
    fn snake_case_security_converts_to_camel_case_yaml() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "secure-api"
version = "0.1.0"
[deploy]
replicas  = 1
mesh      = "none"
namespace = "demo"
[resources]
cpu    = "100m"
memory = "128Mi"

[security.pod]
run_as_non_root = true
run_as_user     = 65532
fs_group        = 65532

[security.container]
allow_privilege_escalation = false
read_only_root_filesystem  = true

[security.container.capabilities]
drop = ["ALL"]

[security.container.seccomp_profile]
type = "RuntimeDefault"
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("podSecurityContext:"), "{values}");
        assert!(values.contains("runAsNonRoot: true"), "{values}");
        assert!(values.contains("runAsUser: 65532"), "{values}");
        assert!(values.contains("fsGroup: 65532"), "{values}");
        assert!(values.contains("containerSecurityContext:"), "{values}");
        assert!(
            values.contains("allowPrivilegeEscalation: false"),
            "{values}"
        );
        assert!(values.contains("readOnlyRootFilesystem: true"), "{values}");
        assert!(values.contains("capabilities:"), "{values}");
        assert!(values.contains("drop:"), "{values}");
        assert!(values.contains("ALL"), "{values}");
        assert!(values.contains("seccompProfile:"), "{values}");
        assert!(values.contains("type: RuntimeDefault"), "{values}");

        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(
            deployment.contains("with .Values.podSecurityContext"),
            "{deployment}"
        );
        assert!(
            deployment.contains("with .Values.containerSecurityContext"),
            "{deployment}"
        );
    }

    #[test]
    fn camel_case_security_keys_pass_through_unchanged() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "secure-api"
version = "0.1.0"
[deploy]
replicas  = 1
mesh      = "none"
namespace = "demo"
[resources]
cpu    = "100m"
memory = "128Mi"

[security.pod]
runAsNonRoot = true
runAsUser    = 1000
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("runAsNonRoot: true"), "{values}");
        assert!(values.contains("runAsUser: 1000"), "{values}");
        assert!(!values.contains("containerSecurityContext:"), "{values}");
    }

    #[test]
    fn no_security_section_omits_security_context_keys() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "bare"
version = "0.1.0"
[deploy]
replicas  = 1
mesh      = "none"
namespace = "demo"
[resources]
cpu    = "50m"
memory = "64Mi"
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(!values.contains("podSecurityContext"), "{values}");
        assert!(!values.contains("containerSecurityContext"), "{values}");
    }

    #[test]
    fn image_registry_from_toml_used_in_generated_values() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "my-api"
version = "2.0.0"
[deploy]
replicas  = 1
mesh      = "none"
namespace = "demo"
[resources]
cpu    = "50m"
memory = "64Mi"

[image]
registry = "ghcr.io/myorg"
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        if std::env::var("TONIN_IMAGE_PREFIX").is_err() {
            assert!(values.contains("ghcr.io/myorg/my-api"), "{values}");
        }
    }

    #[test]
    fn mcp_defaults_to_in_process_mode() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();
        let values = read(&out.join("values.yaml"));
        // RICH_TOML enables mcp_sidecar; the chart defaults to in-process (same
        // binary, two ports) — not a separate <name>-mcp sidecar container.
        assert!(values.contains("mode: in-process"));
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(deployment.contains(r#"eq .Values.mcp.mode "in-process""#));
        assert!(deployment.contains(r#"eq .Values.mcp.mode "sidecar""#));
    }

    #[test]
    fn cilium_mesh_emits_pod_annotations_and_escape_hatches() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        // Cilium transparent encryption is generated, not hand-edited.
        assert!(values.contains("io.cilium/encryption"));
        // Escape hatches default empty so regeneration never clobbers them.
        assert!(values.contains("extraEnv: []"));
        assert!(values.contains("extraManifests: []"));
        // The deploy-time extraManifests template ships in the chart.
        assert!(out.join("templates").join("extra-manifests.yaml").exists());
    }

    #[test]
    fn non_cilium_mesh_has_empty_pod_annotations() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "greeter"
version = "0.1.0"
[deploy]
replicas  = 1
mesh      = "istio"
namespace = "demo"
[resources]
cpu    = "50m"
memory = "64Mi"
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();
        let values = read(&out.join("values.yaml"));
        assert!(values.contains("podAnnotations: {}"));
        assert!(!values.contains("io.cilium/encryption"));
    }

    #[test]
    fn service_without_stateful_deps_disables_blocks() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "greeter"
version = "0.1.0"
[deploy]
replicas  = 1
mesh      = "none"
namespace = "demo"
[resources]
cpu    = "50m"
memory = "64Mi"
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("enabled: false")); // database/cache off
        assert!(values.contains("statefulEnv: {}"));
        assert!(values.contains("keys: []"));
        // mesh=none -> no cilium policy.
        assert!(values.contains("networkPolicy:\n  enabled: false"));
    }

    const HTTP_TOML: &str = r#"
schema = "v1"
[service]
name    = "web-api"
version = "0.1.0"
type    = "http"
port    = 7001
[service.health]
path = "/healthz"
[deploy]
replicas  = 1
mesh      = "cilium"
namespace = "demo"
[resources]
cpu    = "100m"
memory = "128Mi"
"#;

    #[test]
    fn http_service_uses_http_port_name_probe_and_no_mcp() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), HTTP_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("portName: http"));
        assert!(values.contains("port: 7001"));
        assert!(
            values.contains("httpPort: 0"),
            "http-only has no secondary port"
        );
        // [service.health] customizes the probe.
        assert!(values.contains("path: \"/healthz\""));
        // http forces the mcp sidecar off (resolved in tonin-plugin).
        assert!(values.contains("mcp:\n  enabled: false"));
        // The chart ships the httpGet probe + parameterized port name.
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(deployment.contains("livenessProbe"));
        assert!(deployment.contains(".Values.service.portName"));
        assert!(deployment.contains(".Values.service.health.enabled"));
    }

    #[test]
    fn backend_gets_native_grpc_probe_by_default() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("portName: grpc"));
        assert!(
            values.contains("httpPort: 0"),
            "gRPC backend has no http port"
        );
        // A pure gRPC backend gets an auto native grpc: probe on the gRPC port
        // (httpGet on a gRPC port can never succeed).
        assert!(values.contains("health:\n    enabled: true"), "{values}");
        assert!(
            values.contains("grpc: true"),
            "gRPC service uses a grpc: probe: {values}"
        );
        // The deployment template branches to a grpc: probe on health.grpc.
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(
            deployment.contains(".Values.service.health.grpc"),
            "{deployment}"
        );
    }

    #[test]
    fn backend_with_http_block_renders_secondary_port_and_probe() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "collector"
version = "0.1.0"
[service.http]
port = 8081
health_path = "/health"
[deploy]
replicas  = 1
mesh      = "cilium"
namespace = "demo"
[resources]
cpu    = "100m"
memory = "128Mi"
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("portName: grpc"), "gRPC stays the primary");
        assert!(values.contains("port: 50051"));
        assert!(
            values.contains("httpPort: 8081"),
            "secondary http port exposed"
        );
        assert!(values.contains("path: \"/health\""));
        assert!(values.contains("health:\n    enabled: true"));
    }

    #[test]
    fn grpc_backend_values_have_empty_path_and_grpc_true() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(
            values.contains("grpc: true"),
            "gRPC backend must emit grpc:true"
        );
        assert!(
            values.contains("path: \"\""),
            "gRPC backend emits empty path: {values}"
        );
    }

    #[test]
    fn http_service_values_have_non_empty_path_and_grpc_false() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), HTTP_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(
            values.contains("grpc: false"),
            "HTTP service must emit grpc:false"
        );
        assert!(
            !values.contains("path: \"\""),
            "HTTP service must not emit empty path: {values}"
        );
    }

    #[test]
    fn deployment_template_contains_fail_guard_for_grpc_false_empty_path() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(
            deployment.contains("service.health.path"),
            "deployment must reference health.path: {deployment}"
        );
        assert!(
            deployment.contains("fail"),
            "deployment must contain the fail guard: {deployment}"
        );
        assert!(
            deployment.contains("quote"),
            "deployment must quote the path interpolation: {deployment}"
        );
    }

    #[test]
    fn per_env_depends_on_resolves_namespaces_into_values() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "orders"
version = "0.1.0"
type    = "http"
port    = 7001
[deploy]
namespace = "orders-{env}"
replicas  = 1
mesh      = "cilium"
[resources]
cpu    = "100m"
memory = "256Mi"
[depends_on]
identity = "platform-{env}"
billing  = { namespace = "billing-{env}", prod = "billing-shared" }
audit    = { namespace = "security-{env}", envs = ["prod"] }
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["dev".into(), "prod".into()],
        })
        .unwrap();

        // prod: {env} → prod, per-env override wins, prod-only dep present.
        let prod = read(&out.join("values-prod.yaml"));
        assert!(
            prod.contains("- name: identity\n      namespace: platform-prod"),
            "{prod}"
        );
        assert!(
            prod.contains("- name: billing\n      namespace: billing-shared"),
            "{prod}"
        );
        assert!(
            prod.contains("- name: audit\n      namespace: security-prod"),
            "{prod}"
        );

        // dev: default namespace (not the prod override), prod-only dep absent.
        let dev = read(&out.join("values-dev.yaml"));
        assert!(
            dev.contains("- name: identity\n      namespace: platform-dev"),
            "{dev}"
        );
        assert!(
            dev.contains("- name: billing\n      namespace: billing-dev"),
            "{dev}"
        );
        assert!(
            !dev.contains("billing-shared"),
            "prod override must not leak into dev: {dev}"
        );
        assert!(!dev.contains("name: audit"), "audit is prod-only: {dev}");
    }

    #[test]
    fn python_service_with_depends_on_gets_proxy_sidecar_mcp() {
        const PYTHON_TOML: &str = r#"
schema = "v1"
[service]
name     = "notifier"
version  = "0.1.0"
language = "python"

[deploy]
replicas  = 1
mesh      = "none"
namespace = "shop"

[resources]
cpu    = "50m"
memory = "64Mi"

[depends_on]
orders = "shop"
"#;
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), PYTHON_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        // Proxy injected — Python + depends_on
        assert!(
            values.contains("outboundProxy:"),
            "outboundProxy section must be present: {values}"
        );
        assert!(
            values.contains("enabled: true"),
            "proxy must be enabled: {values}"
        );
        assert!(
            values.contains("port: 6565"),
            "proxy port must be 6565: {values}"
        );
        // MCP is sidecar for Python
        assert!(
            values.contains("mode: sidecar"),
            "Python gets sidecar MCP: {values}"
        );

        // deployment.yaml must include the tonin-proxy container block
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(
            deployment.contains("tonin-proxy"),
            "deployment must reference tonin-proxy: {deployment}"
        );
        assert!(
            deployment.contains("TONIN_PROXY_UPSTREAM"),
            "proxy must have upstream env var: {deployment}"
        );
    }

    #[test]
    fn rust_service_gets_no_proxy_and_in_process_mcp() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        // Rust never gets the proxy even though RICH_TOML has depends_on
        assert!(
            values.contains("outboundProxy:"),
            "outboundProxy section present: {values}"
        );
        assert!(
            values.contains("enabled: false"),
            "proxy must be disabled for Rust: {values}"
        );
        assert!(
            values.contains("mode: in-process"),
            "Rust gets in-process MCP: {values}"
        );
    }
}
