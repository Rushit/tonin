//! `tonin helm generate` — render a Helm chart from `tonin.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};
use std::collections::BTreeMap;
use tera::Tera;

use serde::Serialize;
use tonin_plugin::{
    Caller, CallerSource, DatastoreEnv, DependencyRef, EgressSpec, McpMode, MigrationMode, Plan,
    ResourcesSpec, SecuritySection, ServiceKind, VolumeSource, VolumeSpec, WorkloadSpec,
};

static TEMPLATES: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/templates/helm");

/// Chart files (relative to the chart directory) that earlier tonin versions
/// generated and this version no longer ships. `generate` deletes them so a
/// stale template never keeps rendering; `helm check` reports them as drift.
///
/// Only ever list paths tonin itself wrote: everything else under `chart/`
/// (hand-written templates, extra values files) belongs to the user and is
/// never touched.
pub(crate) const OBSOLETE_FILES: &[&str] = &[
    // 0.15: databases and caches are platform-contract references; tonin no
    // longer renders Postgres/Redis StatefulSets and Services.
    "templates/db-statefulset.yaml",
    "templates/db-service.yaml",
    "templates/cache-statefulset.yaml",
    "templates/cache-service.yaml",
];

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

    /// Environment to generate `values-<env>.yaml` for. Repeat for each
    /// environment (`--env dev --env prd`). Required: there is no default set.
    #[arg(long = "env", value_name = "ENV", required = true)]
    pub envs: Vec<String>,
}

pub fn run(args: GenerateArgs) -> Result<()> {
    let service_dir = resolve_service_dir(args.path.as_deref())?;
    let toml_path = service_dir.join("tonin.toml");

    anyhow::ensure!(
        !args.envs.is_empty(),
        "no environment given: pass --env <ENV> once per environment (e.g. --env dev --env prd)"
    );

    // Chart.yaml and the base values.yaml only use env-independent fields
    // (see `base_context`), so any env's plan yields the same output.
    let base_plan = Plan::load_with_env(&toml_path, &args.envs[0])
        .with_context(|| format!("loading {}", toml_path.display()))?;

    let chart_dir = args.out.unwrap_or_else(|| service_dir.join("chart"));
    let templates_dir = chart_dir.join("templates");
    std::fs::create_dir_all(&templates_dir)
        .with_context(|| format!("creating {}", templates_dir.display()))?;

    // --- Chart.yaml --------------------------------------------------------
    let ctx = base_context(&base_plan);
    render_tmpl("Chart.yaml.tmpl", &ctx, &chart_dir.join("Chart.yaml"))?;

    // --- values.yaml (base) ------------------------------------------------
    render_tmpl("values.yaml.tmpl", &ctx, &chart_dir.join("values.yaml"))?;

    // --- values-<env>.yaml -------------------------------------------------
    for env_name in &args.envs {
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

    // --- files earlier tonin versions shipped -------------------------------
    for removed in remove_obsolete(&chart_dir, OBSOLETE_FILES)? {
        println!("removed obsolete {}", removed.display());
    }

    println!("chart generated at {}", chart_dir.display());
    Ok(())
}

fn build_context(plan: &Plan) -> tera::Context {
    let mut ctx = tera::Context::new();
    ctx.insert("name", &plan.name);
    ctx.insert("version", &plan.version);
    ctx.insert("image_repository", &image_repository(&plan.image));
    ctx.insert("replicas", &plan.replicas);
    let autoscale = plan.workload.autoscale;
    ctx.insert(
        "min_replicas",
        &autoscale.map_or(plan.replicas, |a| a.min_replicas),
    );
    ctx.insert("max_replicas", &plan.max_replicas);
    ctx.insert("has_autoscale", &autoscale.is_some());
    ctx.insert("mcp_enabled", &(plan.mcp_mode != McpMode::None));
    ctx.insert("mcp_mode", &plan.mcp_mode.as_str());
    ctx.insert("needs_proxy", &plan.needs_outbound_proxy);
    ctx.insert("proxy_port", &6565_u16);
    ctx.insert(
        "resources_block",
        &resources_block(&plan.workload.resources),
    );
    workload_context(&plan.workload, &mut ctx);
    ctx.insert("namespace", &plan.namespace);
    ctx.insert("mesh", plan.mesh.as_str());
    ctx.insert("is_cilium", &(plan.mesh.as_str() == "cilium"));
    ctx.insert("expose", &plan.expose);

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

    // ---- Databases and caches: platform-contract references, resolved for
    //      this env (Secret key or dev url → env var, CA mount, egress).
    ctx.insert(
        "datastores_block",
        &datastores_block(&plan.emitted_env.datastores),
    );
    // ---- Keys of the service's own Secret ([secrets].required).
    ctx.insert("secret_keys", &plan.emitted_env.from_secret);

    // ---- Plain env vars from [env] (+ [env.<env>] overlay), resolved for
    //      this env. Rendered into the chart's `env:` map, which the
    //      deployment template already injects into the server container.
    ctx.insert("env_vars", &plan.env_vars);

    // ---- Migrations: `[migrations] mode` (+ `[migrations.<env>]`) selects
    //      the initContainer, the pre-install/pre-upgrade hook Job, or nothing.
    let migrations_mode = plan
        .migrations
        .as_ref()
        .map_or(MigrationMode::None, |m| m.mode);
    ctx.insert("migrations_mode", migrations_mode.as_str());
    let migrations_command: Vec<String> = plan
        .migrations
        .as_ref()
        .map(|m| m.command.clone())
        .unwrap_or_default();
    ctx.insert("migrations_command", &migrations_command);

    // ---- CiliumNetworkPolicy: ingress ([callers]) and egress
    //      ([depends_on], [egress]); datastore egress comes from `datastores`.
    ctx.insert(
        "network_policy_block",
        &network_policy_block(&NetworkPolicyValues::for_plan(plan)),
    );

    ctx.insert("env_name", &plan.selected_env);

    plan.security.as_ref().contribute(&mut ctx);

    ctx
}

/// Context for `Chart.yaml` and the base `values.yaml`: env-independent
/// fields of `plan` only. Every key whose value depends on the environment
/// (an `[<section>.<env>]` overlay or an `{env}` placeholder) gets a neutral
/// default here; `values-<env>.yaml` carries the real value. This keeps the
/// base free of one environment's settings (e.g. dev's shared-instance URLs).
fn base_context(plan: &Plan) -> tera::Context {
    let mut ctx = build_context(plan);
    // [deploy] (+ overlays): replicas, autoscaling, mesh, MCP, ingress.
    ctx.insert("replicas", &1_u32);
    ctx.insert("min_replicas", &1_u32);
    ctx.insert("max_replicas", &1_u32);
    // [deploy] strategy / termination_grace_period_seconds (+ overlays).
    ctx.insert("strategy_block", "strategy: {}");
    ctx.insert("termination_grace_period_seconds", &0_u32);
    ctx.insert("has_autoscale", &false);
    ctx.insert("namespace", "");
    ctx.insert("mesh", "none");
    ctx.insert("is_cilium", &false);
    ctx.insert("expose", &Option::<String>::None);
    ctx.insert("mcp_enabled", &false);
    ctx.insert("mcp_mode", McpMode::None.as_str());
    // Derived from [depends_on].
    ctx.insert("needs_proxy", &false);
    // [database] / [cache] / [secrets] / [env] / [migrations] mode /
    // [callers] / [depends_on].
    ctx.insert("datastores_block", &datastores_block(&[]));
    ctx.insert("secret_keys", &Vec::<String>::new());
    ctx.insert("env_vars", &Vec::<(String, String)>::new());
    ctx.insert("migrations_mode", MigrationMode::None.as_str());
    ctx.insert(
        "network_policy_block",
        &network_policy_block(&NetworkPolicyValues {
            enabled: false,
            port: plan.port,
            mcp: false,
            callers: Vec::new(),
            depends_on: Vec::new(),
            egress: EgressSpec::default(),
        }),
    );
    ctx.insert("env_name", "");
    ctx
}

/// Workload keys: kind, StatefulSet settings, strategy, command/args,
/// probes, volumes and extra ports.
fn workload_context(w: &WorkloadSpec, ctx: &mut tera::Context) {
    ctx.insert("workload_kind", w.kind.as_str());
    ctx.insert(
        "pod_management_policy",
        w.pod_management_policy.as_deref().unwrap_or(""),
    );
    ctx.insert("strategy_block", &strategy_block(w));
    ctx.insert(
        "termination_grace_period_seconds",
        &w.termination_grace_period_seconds.unwrap_or(0),
    );
    ctx.insert("command_json", &json_list(&w.command));
    ctx.insert("args_json", &json_list(&w.args));
    ctx.insert("liveness_path", &w.probes.liveness_path);
    ctx.insert("readiness_path", &w.probes.readiness_path);
    ctx.insert("startup_enabled", &w.probes.startup.is_some());
    let startup = w
        .probes
        .startup
        .clone()
        .unwrap_or(tonin_plugin::StartupProbeSpec {
            path: w.probes.liveness_path.clone(),
            period_seconds: 10,
            failure_threshold: 30,
        });
    ctx.insert("startup_path", &startup.path);
    ctx.insert("startup_period_seconds", &startup.period_seconds);
    ctx.insert("startup_failure_threshold", &startup.failure_threshold);
    ctx.insert("volumes_block", &volumes_block(&w.volumes));
    ctx.insert(
        "extra_ports_block",
        &yaml_list_block("extraPorts", &w.ports),
    );
    ctx.insert(
        "extra_services_block",
        &yaml_list_block("extraServices", &w.extra_services()),
    );
}

/// A JSON array (valid YAML flow sequence) of strings.
fn json_list(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}

/// `key: []` or `key:` followed by the list as indented YAML.
fn yaml_list_block<T: serde::Serialize>(key: &str, items: &[T]) -> String {
    if items.is_empty() {
        return format!("{key}: []");
    }
    let yaml = serde_yaml::to_string(items).unwrap_or_default();
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    let body: Vec<String> = yaml.lines().map(|l| format!("  {l}")).collect();
    format!("{key}:\n{}", body.join("\n"))
}

/// `strategy:` values key, in Kubernetes shape (`{}` = the controller's default).
fn strategy_block(w: &WorkloadSpec) -> String {
    match &w.strategy {
        None => "strategy: {}".into(),
        Some(s) => {
            let yaml = serde_yaml::to_string(s).unwrap_or_default();
            let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
            let body: Vec<String> = yaml.lines().map(|l| format!("    {l}")).collect();
            format!("strategy:\n{}", body.join("\n"))
        }
    }
}

/// The container `resources:` values key.
fn resources_block(r: &ResourcesSpec) -> String {
    let mut out = String::from("resources:");
    for (name, list) in [("requests", &r.requests), ("limits", &r.limits)] {
        if list.is_empty() {
            continue;
        }
        out.push_str(&format!("\n  {name}:"));
        for (k, v) in list {
            out.push_str(&format!("\n    {k}: \"{v}\""));
        }
    }
    if r.requests.is_empty() && r.limits.is_empty() {
        out.push_str(" {}");
    }
    out
}

/// The `volumes:` values key ([[volumes]]): `type` selects the pod volume
/// (`empty-dir`) or the StatefulSet claim template (`pvc`).
fn volumes_block(volumes: &[VolumeSpec]) -> String {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Entry<'a> {
        name: &'a str,
        #[serde(rename = "type")]
        kind: &'a str,
        mount_path: &'a str,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        read_only: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        empty_dir: Option<std::collections::BTreeMap<&'a str, &'a str>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        storage_class: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        access_modes: Option<&'a [String]>,
    }
    let entries: Vec<Entry<'_>> = volumes
        .iter()
        .map(|v| match &v.source {
            VolumeSource::EmptyDir { size_limit, medium } => {
                let mut ed = std::collections::BTreeMap::new();
                if let Some(m) = medium {
                    ed.insert("medium", m.as_str());
                }
                if let Some(l) = size_limit {
                    ed.insert("sizeLimit", l.as_str());
                }
                Entry {
                    name: &v.name,
                    kind: "empty-dir",
                    mount_path: &v.mount_path,
                    read_only: v.read_only,
                    empty_dir: Some(ed),
                    size: None,
                    storage_class: None,
                    access_modes: None,
                }
            }
            VolumeSource::Pvc {
                size,
                storage_class,
                access_modes,
            } => Entry {
                name: &v.name,
                kind: "pvc",
                mount_path: &v.mount_path,
                read_only: v.read_only,
                empty_dir: None,
                size: Some(size),
                storage_class: storage_class.as_deref(),
                access_modes: Some(access_modes),
            },
        })
        .collect();
    yaml_list_block("volumes", &entries)
}

/// The `datastores:` values key: `datastores: []`, or the resolved list as
/// YAML (keys in the order the chart templates read them).
fn datastores_block(datastores: &[DatastoreEnv]) -> String {
    if datastores.is_empty() {
        return "datastores: []".into();
    }
    let yaml = serde_yaml::to_string(datastores).unwrap_or_default();
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    let body: Vec<String> = yaml.lines().map(|l| format!("  {l}")).collect();
    format!("datastores:\n{}", body.join("\n"))
}

/// The `networkPolicy:` values key, read by `networkpolicy.yaml`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkPolicyValues {
    enabled: bool,
    /// Main service port (part of every caller's default port set).
    port: u32,
    /// MCP port in the default port set.
    mcp: bool,
    callers: Vec<CallerValues>,
    depends_on: Vec<DependencyValues>,
    egress: EgressSpec,
}

/// One ingress rule. A service caller is matched by
/// `service.identity: <name>.<namespace>`; a caller with `labels` by those
/// pod labels (in `namespace`, or the release namespace when absent).
/// Empty `ports` means the default set (main + HTTP + MCP port).
#[derive(Serialize)]
struct CallerValues {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<u32>,
}

impl From<&Caller> for CallerValues {
    fn from(c: &Caller) -> Self {
        match &c.source {
            CallerSource::Service(s) => Self {
                name: s.name.clone(),
                namespace: Some(s.namespace.clone()),
                labels: BTreeMap::new(),
                ports: c.ports.clone(),
            },
            CallerSource::Pods { namespace, labels } => Self {
                name: c.name.clone(),
                namespace: namespace.clone(),
                labels: labels.clone(),
                ports: c.ports.clone(),
            },
        }
    }
}

/// One egress rule to another tonin service (`service.identity`).
#[derive(Serialize)]
struct DependencyValues {
    name: String,
    namespace: String,
    ports: Vec<u32>,
}

impl From<&DependencyRef> for DependencyValues {
    fn from(d: &DependencyRef) -> Self {
        Self {
            name: d.name.clone(),
            namespace: d.namespace.clone(),
            ports: d.ports.clone(),
        }
    }
}

impl NetworkPolicyValues {
    fn for_plan(plan: &Plan) -> Self {
        Self {
            enabled: plan.mesh.as_str() == "cilium",
            port: plan.port,
            mcp: plan.mcp_mode != McpMode::None,
            callers: plan.callers.iter().map(CallerValues::from).collect(),
            depends_on: plan.depends_on.iter().map(DependencyValues::from).collect(),
            egress: plan.egress.clone(),
        }
    }
}

/// The `networkPolicy:` values key as YAML.
fn network_policy_block(values: &NetworkPolicyValues) -> String {
    #[derive(Serialize)]
    struct Wrapper<'a> {
        #[serde(rename = "networkPolicy")]
        network_policy: &'a NetworkPolicyValues,
    }
    let yaml = serde_yaml::to_string(&Wrapper {
        network_policy: values,
    })
    .unwrap_or_default();
    yaml.strip_prefix("---\n")
        .unwrap_or(&yaml)
        .trim_end()
        .to_string()
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

/// The files on `obsolete` (paths relative to `chart_dir`) that exist there.
pub(crate) fn obsolete_present(chart_dir: &Path, obsolete: &[&str]) -> Vec<PathBuf> {
    obsolete
        .iter()
        .map(PathBuf::from)
        .filter(|rel| chart_dir.join(rel).is_file())
        .collect()
}

/// Delete the files on `obsolete` from `chart_dir`; returns the removed paths
/// (relative to `chart_dir`). Files not on the list are never touched.
fn remove_obsolete(chart_dir: &Path, obsolete: &[&str]) -> Result<Vec<PathBuf>> {
    let present = obsolete_present(chart_dir, obsolete);
    for rel in &present {
        let path = chart_dir.join(rel);
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(present)
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

    /// A `tonin.toml` exercising every stateful surface: contract DB + cache
    /// in prod, literal dev URLs, app secrets, callers (with a dev overlay),
    /// migrations (a Job in prod), and the cilium mesh.
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
url = "postgresql://u:p@postgres.shared-dev.svc.cluster.local:5432/identity_dev"

[cache]
engine = "redis"

[cache.dev]
url = "redis://redis.shared-dev.svc.cluster.local:6379"

[secrets]
required = ["JWT_SIGNING_KEY"]

[migrations]
tool = "sqlx"
dir  = "migrations/"
mode = "init-container"

[migrations.prod]
mode = "job"

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
            "secret.yaml",
            "networkpolicy.yaml",
            "migration-job.yaml",
        ] {
            assert!(
                out.join("templates").join(f).exists(),
                "missing template {f}"
            );
        }
        // Databases and caches are platform-contract references: nothing renders them.
        for f in OBSOLETE_FILES {
            assert!(!out.join(f).exists(), "{f} must not be generated");
        }
        assert!(out.join("values.yaml").exists());
        assert!(out.join("values-dev.yaml").exists());
        assert!(out.join("values-prod.yaml").exists());
    }

    #[test]
    fn every_template_renders_the_image_through_the_helper() {
        let dir = TEMPLATES.get_dir("chart-templates").unwrap();
        for file in dir.files() {
            let name = file.path().file_name().unwrap().to_string_lossy();
            if name == "_helpers.tpl" {
                continue;
            }
            let src = file.contents_utf8().unwrap();
            assert!(
                !src.contains(".Values.image.repository"),
                "{name} builds the image reference itself; use `include \"service.image\" .`"
            );
        }
        let helpers = dir
            .files()
            .find(|f| f.path().ends_with("_helpers.tpl"))
            .unwrap()
            .contents_utf8()
            .unwrap();
        assert!(helpers.contains(r#"define "service.image""#));
        assert!(helpers.contains(".Values.image.digest"));
    }

    #[test]
    fn obsolete_list_never_names_a_shipped_file() {
        let shipped: Vec<String> = TEMPLATES
            .get_dir("chart-templates")
            .unwrap()
            .files()
            .map(|f| {
                format!(
                    "templates/{}",
                    f.path().file_name().unwrap().to_string_lossy()
                )
            })
            .chain(["Chart.yaml".to_string(), "values.yaml".to_string()])
            .collect();
        for obsolete in OBSOLETE_FILES {
            assert!(
                !shipped.iter().any(|s| s == obsolete),
                "{obsolete} is still generated"
            );
            assert!(
                !obsolete.starts_with("values-"),
                "values-<env>.yaml files may be user-owned"
            );
        }
    }

    #[test]
    fn remove_obsolete_deletes_only_listed_tonin_files() {
        let chart = tempfile::tempdir().unwrap();
        let templates = chart.path().join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        for f in [
            "db-statefulset.yaml",
            "custom-cronjob.yaml",
            "deployment.yaml",
        ] {
            std::fs::write(templates.join(f), "kind: X\n").unwrap();
        }

        let removed = remove_obsolete(
            chart.path(),
            &[
                "templates/db-statefulset.yaml",
                "templates/cache-service.yaml",
            ],
        )
        .unwrap();

        assert_eq!(
            removed,
            vec![PathBuf::from("templates/db-statefulset.yaml")]
        );
        assert!(!templates.join("db-statefulset.yaml").exists());
        // Hand-written and still-shipped files survive.
        assert!(templates.join("custom-cronjob.yaml").exists());
        assert!(templates.join("deployment.yaml").exists());
    }

    #[test]
    fn base_values_are_identical_for_every_env() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let render = |env: &str| {
            let out = svc.path().join(format!("chart-{env}"));
            run(GenerateArgs {
                path: Some(svc.path().to_path_buf()),
                out: Some(out.clone()),
                envs: vec![env.into()],
            })
            .unwrap();
            (
                read(&out.join("values.yaml")),
                read(&out.join("Chart.yaml")),
            )
        };
        let (dev, dev_chart) = render("dev");
        let (prod, prod_chart) = render("prod");
        assert_eq!(dev, prod, "base values.yaml must not depend on --env");
        assert_eq!(dev_chart, prod_chart);

        // Nothing from any overlay or env-resolved section leaks into the base.
        for leaked in [
            "shared-dev",
            "myapp-dev",
            "orders-service",
            "identity-pg-app",
            "cnpg.io/cluster",
            "JWT_SIGNING_KEY",
            "io.cilium",
        ] {
            assert!(!dev.contains(leaked), "base leaks {leaked:?}:\n{dev}");
        }
        assert!(dev.contains("replicaCount: 1"), "{dev}");
        assert!(dev.contains("mesh: \"none\""), "{dev}");
        assert!(dev.contains("\ndatastores: []"), "{dev}");
        assert!(dev.contains("  mode: none"), "{dev}");
        assert!(dev.contains("networkPolicy:\n  enabled: false"), "{dev}");

        // values-<env>.yaml carries the env-specific settings instead.
        let prod_env = read(&svc.path().join("chart-prod/values-prod.yaml"));
        assert!(prod_env.contains("replicaCount: 2"), "{prod_env}");
        assert!(prod_env.contains("mesh: \"cilium\""), "{prod_env}");
        assert!(prod_env.contains("mode: in-process"), "{prod_env}");
        assert!(prod_env.contains("name: identity-pg-app"), "{prod_env}");
    }

    #[test]
    fn prod_values_reference_the_platform_contract() {
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
        // DATABASE_URL / REDIS_URL come from the contract Secrets' `uri` key.
        for expected in [
            "- name: database\n    env: DATABASE_URL\n    secret:\n      env: DATABASE_URL\n      name: identity-pg-app\n      key: uri",
            "- name: cache\n    env: REDIS_URL\n    secret:\n      env: REDIS_URL\n      name: identity-cache\n      key: uri",
            "cnpg.io/cluster: identity-pg",
            "app.kubernetes.io/name: identity-cache",
            "port: 5432",
            "port: 6379",
        ] {
            assert!(prod.contains(expected), "missing {expected:?}:\n{prod}");
        }
        // tonin no longer adds a password key to the service's own Secret.
        assert!(!prod.contains("DATABASE_PASSWORD"), "{prod}");
        assert!(prod.contains("- JWT_SIGNING_KEY"));
        // Cilium policy allowlist resolves the base caller namespace.
        assert!(prod.contains("name: orders-service"));
        assert!(prod.contains("namespace: myapp"));
    }

    #[test]
    fn dev_values_use_literal_urls_instead_of_contract_secrets() {
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
        assert!(
            dev.contains(
                "env: DATABASE_URL\n    value: postgresql://u:p@postgres.shared-dev.svc.cluster.local:5432/identity_dev"
            ),
            "{dev}"
        );
        assert!(
            dev.contains("value: redis://redis.shared-dev.svc.cluster.local:6379"),
            "{dev}"
        );
        assert!(!dev.contains("identity-pg-app"), "{dev}");
        assert!(!dev.contains("DATABASE_PASSWORD"), "{dev}");
        // Caller dev overlay applies.
        assert!(dev.contains("namespace: myapp-dev"));
    }

    #[test]
    fn migrations_mode_is_per_env_and_ships_job_template() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["dev".into(), "prod".into()],
        })
        .unwrap();

        let values = read(&out.join("values.yaml"));
        assert!(values.contains("  mode: none"), "{values}"); // env-neutral base
        assert!(values.contains("- \"sqlx\""), "{values}");
        assert!(values.contains("env: {}")); // migration-only env hatch
        let dev = read(&out.join("values-dev.yaml"));
        assert!(dev.contains("migrations:\n  mode: init-container"), "{dev}");
        let prod = read(&out.join("values-prod.yaml"));
        assert!(prod.contains("migrations:\n  mode: job"), "{prod}");

        let job = read(&out.join("templates").join("migration-job.yaml"));
        assert!(job.contains("helm.sh/hook\": pre-install,pre-upgrade"));
        assert!(job.contains(r#"eq .Values.migrations.mode "job""#));
        // The Job's pods are not selected by the Service (`app`), but carry
        // service.identity so the service's network policy applies.
        assert!(job.contains("app.kubernetes.io/component: migrate"));
        assert!(job.contains("service.identity:"));
        // Its network policy is a hook that runs before the Job.
        assert!(job.contains("\"helm.sh/hook-weight\": \"-5\""), "{job}");
    }

    #[test]
    fn generate_removes_the_db_and_cache_templates_of_earlier_versions() {
        let svc = tempfile::tempdir().unwrap();
        write_service(svc.path(), RICH_TOML);
        let out = svc.path().join("chart");
        let templates = out.join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        for f in OBSOLETE_FILES {
            std::fs::write(out.join(f), "kind: StatefulSet\n").unwrap();
        }
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();
        for f in OBSOLETE_FILES {
            assert!(!out.join(f).exists(), "{f} survived regeneration");
        }
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

        // Secret-sourced env precedes the datastore env and the plain env, so
        // their values can use $(VAR) expansion.
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        let secrets_at = deployment.find(".Values.secrets.keys").unwrap();
        let datastores_at = deployment.find("\"service.datastoreEnv\"").unwrap();
        let env_at = deployment.find(".Values.env }}").unwrap();
        assert!(secrets_at < datastores_at && datastores_at < env_at);
        // Within a datastore, the Secret-sourced (helper) var comes first.
        let helper = read(&out.join("templates").join("_helpers.tpl"));
        let def = helper.find("define \"service.datastoreEnv\"").unwrap();
        let secret_at = def + helper[def..].find("secretKeyRef").unwrap();
        let value_at = def + helper[def..].find("value: {{ .value").unwrap();
        assert!(secret_at < value_at);
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
        let values = read(&out.join("values-prod.yaml"));
        // RICH_TOML enables mcp_sidecar; the chart defaults to in-process (same
        // binary, two ports) — not a separate <name>-mcp sidecar container.
        assert!(values.contains("mode: in-process"));
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(deployment.contains(r#"eq .Values.mcp.mode "in-process""#));
        assert!(deployment.contains(r#"eq .Values.mcp.mode "sidecar""#));
    }

    #[test]
    fn cilium_mesh_has_no_pod_annotations_and_keeps_escape_hatches() {
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
        // No mesh-derived pod annotations: Cilium transparent encryption is a
        // cluster setting, not a per-pod annotation (and is off by design).
        assert!(values.contains("podAnnotations: {}"), "{values}");
        let prod = read(&out.join("values-prod.yaml"));
        assert!(!prod.contains("io.cilium/encryption"), "{prod}");
        assert!(!prod.contains("podAnnotations"), "{prod}");
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
        assert!(values.contains("datastores: []"));
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
            prod.contains("- name: identity\n    namespace: platform-prod"),
            "{prod}"
        );
        assert!(
            prod.contains("- name: billing\n    namespace: billing-shared"),
            "{prod}"
        );
        assert!(
            prod.contains("- name: audit\n    namespace: security-prod"),
            "{prod}"
        );

        // dev: default namespace (not the prod override), prod-only dep absent.
        let dev = read(&out.join("values-dev.yaml"));
        assert!(
            dev.contains("- name: identity\n    namespace: platform-dev"),
            "{dev}"
        );
        assert!(
            dev.contains("- name: billing\n    namespace: billing-dev"),
            "{dev}"
        );
        assert!(
            !dev.contains("billing-shared"),
            "prod override must not leak into dev: {dev}"
        );
        assert!(!dev.contains("name: audit"), "audit is prod-only: {dev}");
    }

    #[test]
    fn env_tables_render_into_values_env_map() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "gateway"
version = "0.1.0"
type    = "http"
port    = 7001
[deploy]
replicas  = 1
mesh      = "cilium"
namespace = "zvectorlabs-{env}"
[resources]
cpu    = "100m"
memory = "128Mi"
[env]
IDENTITY_GRPC_URL = "http://identity.zvectorlabs-{env}.svc.cluster.local:50051"
LOG_FORMAT        = "json"
[env.dev]
LOG_FORMAT = "pretty"
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["dev".into(), "prod".into()],
        })
        .unwrap();

        // {env} resolves per environment; the dev overlay wins in dev only.
        let dev = read(&out.join("values-dev.yaml"));
        assert!(
            dev.contains(
                "IDENTITY_GRPC_URL: \"http://identity.zvectorlabs-dev.svc.cluster.local:50051\""
            ),
            "{dev}"
        );
        assert!(dev.contains("LOG_FORMAT: \"pretty\""), "{dev}");
        let prod = read(&out.join("values-prod.yaml"));
        assert!(
            prod.contains(
                "IDENTITY_GRPC_URL: \"http://identity.zvectorlabs-prod.svc.cluster.local:50051\""
            ),
            "{prod}"
        );
        assert!(prod.contains("LOG_FORMAT: \"json\""), "{prod}");
        // The base values.yaml is env-neutral: [env] is per-env only. The
        // deployment template injects .Values.env into the server container.
        let base = read(&out.join("values.yaml"));
        assert!(!base.contains("IDENTITY_GRPC_URL"), "{base}");
        assert!(base.contains("\nenv: {}"), "{base}");
        let deployment = read(&out.join("templates").join("deployment.yaml"));
        assert!(deployment.contains(".Values.env"), "{deployment}");
    }

    #[test]
    fn no_env_section_renders_empty_env_map() {
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
        // Column-0 anchor: distinguishes from the nested `migrations.env: {}`.
        assert!(values.contains("\nenv: {}"), "{values}");
    }

    #[test]
    fn depends_on_port_renders_into_values_and_policy_template() {
        let svc = tempfile::tempdir().unwrap();
        write_service(
            svc.path(),
            r#"
schema = "v1"
[service]
name    = "zvectorlabs-api"
version = "0.1.0"
type    = "http"
port    = 7001
[deploy]
namespace = "zvectorlabs-{env}"
replicas  = 1
mesh      = "cilium"
[resources]
cpu    = "100m"
memory = "256Mi"
[depends_on]
identity        = "zvectorlabs-{env}"
zradar-platform = { namespace = "zradar-{env}", port = 4317 }
"#,
        );
        let out = svc.path().join("chart");
        run(GenerateArgs {
            path: Some(svc.path().to_path_buf()),
            out: Some(out.clone()),
            envs: vec!["prod".into()],
        })
        .unwrap();

        let prod = read(&out.join("values-prod.yaml"));
        // Bare-string form keeps the historical 50051 default.
        assert!(
            prod.contains(
                "- name: identity\n    namespace: zvectorlabs-prod\n    ports:\n    - 50051"
            ),
            "{prod}"
        );
        // Table form carries its declared port.
        assert!(
            prod.contains(
                "- name: zradar-platform\n    namespace: zradar-prod\n    ports:\n    - 4317"
            ),
            "{prod}"
        );

        let policy = read(&out.join("templates").join("networkpolicy.yaml"));
        assert!(
            policy.contains(r#"(.ports | default (list (.port | default 50051)))"#),
            "egress uses the per-dependency port: {policy}"
        );
        assert!(
            !policy.contains(r#"- { port: "50051", protocol: TCP }"#),
            "hardcoded dependency egress port must be gone: {policy}"
        );
        // DNS egress ships alongside — default-deny would break discovery.
        assert!(policy.contains("k8s-app: kube-dns"), "{policy}");
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

        let base = read(&out.join("values.yaml"));
        assert!(
            base.contains("port: 6565"),
            "proxy port must be 6565: {base}"
        );
        let values = read(&out.join("values-prod.yaml"));
        // Proxy injected — Python + depends_on
        assert!(
            values.contains("outboundProxy:\n  enabled: true"),
            "proxy must be enabled: {values}"
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

        let values = read(&out.join("values-prod.yaml"));
        // Rust never gets the proxy even though RICH_TOML has depends_on
        assert!(
            values.contains("outboundProxy:\n  enabled: false"),
            "proxy must be disabled for Rust: {values}"
        );
        assert!(
            values.contains("mode: in-process"),
            "Rust gets in-process MCP: {values}"
        );
    }
}
