//! Pod hygiene: `[placement]`, `[pdb]`, `[service_account]` and `[[cronjobs]]`.
//!
//! Everything here shapes the pods tonin renders (Deployment, migration Job,
//! CronJobs) without changing what the service does: where pods may run, how
//! many may be evicted at once, which ServiceAccount they run as (and which
//! projected tokens they get), and scheduled jobs that reuse the service image.
//! `[deploy] image_pull_secrets` is resolved in `plan.rs` with the rest of
//! `[deploy]`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::plan::Error;

// ---------- raw (tonin.toml) ----------

/// `[placement]` and its `[placement.<env>]` overlays.
#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawPlacement {
    #[serde(flatten)]
    pub base: RawPlacementFields,
    #[serde(flatten)]
    pub envs: BTreeMap<String, RawPlacementFields>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawPlacementFields {
    #[serde(default)]
    pub node_selector: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub tolerations: Option<Vec<toml::Value>>,
    /// `false` (or `[]`) disables spreading; a list of tables replaces the
    /// default hostname constraint.
    #[serde(default)]
    pub topology_spread: Option<toml::Value>,
}

/// `[pdb]` and its `[pdb.<env>]` overlays.
#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawPdb {
    #[serde(flatten)]
    pub base: RawPdbFields,
    #[serde(flatten)]
    pub envs: BTreeMap<String, RawPdbFields>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawPdbFields {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub min_available: Option<toml::Value>,
    #[serde(default)]
    pub max_unavailable: Option<toml::Value>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawServiceAccount {
    #[serde(default)]
    pub automount_token: Option<bool>,
    /// Shorthand for a single entry of `tokens`.
    #[serde(default)]
    pub token: Option<RawSaToken>,
    #[serde(default)]
    pub tokens: Vec<RawSaToken>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSaToken {
    pub audience: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub expiration_seconds: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCronJob {
    pub name: String,
    pub schedule: String,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub concurrency_policy: Option<String>,
    #[serde(default)]
    pub successful_jobs_history_limit: Option<u32>,
    #[serde(default)]
    pub failed_jobs_history_limit: Option<u32>,
    #[serde(default)]
    pub starting_deadline_seconds: Option<u64>,
    #[serde(default)]
    pub active_deadline_seconds: Option<u64>,
    #[serde(default)]
    pub backoff_limit: Option<u32>,
    #[serde(default)]
    pub suspend: Option<bool>,
    #[serde(default)]
    pub resources: Option<RawCronResources>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Environments the CronJob is rendered in (default: all).
    #[serde(default)]
    pub envs: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCronResources {
    pub cpu: String,
    pub memory: String,
}

// ---------- resolved ----------

/// Pod-level settings shared by every pod tonin renders, resolved for one env.
#[derive(Clone, Debug, Default)]
pub struct PodSettings {
    /// `[deploy] image_pull_secrets` (+ `[deploy.<env>]`): Secret names.
    pub image_pull_secrets: Vec<String>,
    pub placement: PlacementSpec,
    /// `None` when `[pdb]` is absent or disabled for this env.
    pub pdb: Option<PdbSpec>,
    pub service_account: ServiceAccountSpec,
    /// CronJobs rendered in this env (`envs` filter applied).
    pub cronjobs: Vec<CronJobSpec>,
}

/// Where pods may run. Keys of tolerations / spread constraints are camelCase.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacementSpec {
    pub node_selector: BTreeMap<String, String>,
    pub tolerations: Vec<toml::Value>,
    /// Constraints without `labelSelector` get the service's selector labels
    /// in the chart. Rendered only on the service's own pods, and only when
    /// it runs more than one replica.
    pub topology_spread: Vec<toml::Value>,
}

impl Default for PlacementSpec {
    fn default() -> Self {
        Self {
            node_selector: BTreeMap::new(),
            tolerations: Vec::new(),
            topology_spread: vec![default_spread()],
        }
    }
}

/// `maxSkew: 1` over `kubernetes.io/hostname`, `ScheduleAnyway`: prefer one
/// replica per node, never block scheduling.
fn default_spread() -> toml::Value {
    let mut t = toml::map::Map::new();
    t.insert("maxSkew".into(), toml::Value::Integer(1));
    t.insert(
        "topologyKey".into(),
        toml::Value::String("kubernetes.io/hostname".into()),
    );
    t.insert(
        "whenUnsatisfiable".into(),
        toml::Value::String("ScheduleAnyway".into()),
    );
    toml::Value::Table(t)
}

/// An int or a percentage (`"50%"`), as PodDisruptionBudget fields take.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum IntOrPercent {
    Int(u32),
    Percent(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdbSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_available: Option<IntOrPercent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_unavailable: Option<IntOrPercent>,
}

/// The service's own ServiceAccount (named after the service).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceAccountSpec {
    /// `automountServiceAccountToken` (default `false`).
    pub automount_token: bool,
    pub tokens: Vec<SaToken>,
}

/// A projected ServiceAccount token (audience-bound, auto-rotated by the kubelet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaToken {
    pub audience: String,
    /// Absolute file path in the container.
    pub path: String,
    pub expiration_seconds: u32,
}

/// One projected volume: the tokens that share a directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenVolume {
    pub name: String,
    pub mount_path: String,
    pub sources: Vec<TokenSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSource {
    pub audience: String,
    pub expiration_seconds: u32,
    /// File name relative to the volume's mount path.
    pub path: String,
}

impl ServiceAccountSpec {
    /// Tokens grouped into one projected volume per directory (sorted), so
    /// two tokens in `/var/run/secrets/tokens/` share a mount. Never a
    /// `subPath` mount: those do not receive the kubelet's token rotation.
    pub fn token_volumes(&self) -> Vec<TokenVolume> {
        let mut by_dir: BTreeMap<&str, Vec<TokenSource>> = BTreeMap::new();
        for t in &self.tokens {
            let (dir, file) = split_path(&t.path);
            by_dir.entry(dir).or_default().push(TokenSource {
                audience: t.audience.clone(),
                expiration_seconds: t.expiration_seconds,
                path: file.to_string(),
            });
        }
        by_dir
            .into_iter()
            .enumerate()
            .map(|(i, (dir, sources))| TokenVolume {
                name: format!("sa-token-{i}"),
                mount_path: dir.to_string(),
                sources,
            })
            .collect()
    }
}

/// `(dir, file)` of an absolute path; `dir` is `/` for a file at the root.
fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some(("", file)) => ("/", file),
        Some((dir, file)) => (dir, file),
        None => ("/", path),
    }
}

/// A resolved `[[cronjobs]]` entry, serialized as the chart's `cronjobs` values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJobSpec {
    pub name: String,
    pub schedule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Replaces the service's command; `None` keeps it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub concurrency_policy: String,
    pub successful_jobs_history_limit: u32,
    pub failed_jobs_history_limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_deadline_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_deadline_seconds: Option<u64>,
    pub backoff_limit: u32,
    pub suspend: bool,
    /// `None` uses the service's resources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<CronResources>,
    /// Extra env for this job; wins over the service's `[env]`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CronResources {
    pub requests: CpuMemory,
    pub limits: CpuMemory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CpuMemory {
    pub cpu: String,
    pub memory: String,
}

// ---------- resolution ----------

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Error {
    Error::InvalidField {
        field: field.into(),
        reason: reason.into(),
    }
}

/// `snake_case` → `camelCase`; keys without `_` pass through, so camelCase
/// works too (same rule as `[security]`).
fn camel(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut upper = false;
    for c in key.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn camel_keys(val: &toml::Value) -> toml::Value {
    match val {
        toml::Value::Table(t) => {
            toml::Value::Table(t.iter().map(|(k, v)| (camel(k), camel_keys(v))).collect())
        }
        toml::Value::Array(a) => toml::Value::Array(a.iter().map(camel_keys).collect()),
        other => other.clone(),
    }
}

/// A list of tables with camelCase keys, each key in `allowed` and every
/// key of `required` present.
fn object_list(
    field: &str,
    items: &[toml::Value],
    allowed: &[&str],
    required: &[&str],
) -> Result<Vec<toml::Value>, Error> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let item = camel_keys(item);
            let toml::Value::Table(t) = &item else {
                return Err(invalid(format!("{field}[{i}]"), "must be a table"));
            };
            for k in t.keys() {
                if !allowed.contains(&k.as_str()) {
                    return Err(invalid(
                        format!("{field}[{i}].{k}"),
                        format!("unknown key; allowed: {}", allowed.join(", ")),
                    ));
                }
            }
            for r in required {
                if !t.contains_key(*r) {
                    return Err(invalid(format!("{field}[{i}]"), format!("missing `{r}`")));
                }
            }
            Ok(item)
        })
        .collect()
}

const TOLERATION_KEYS: &[&str] = &["key", "operator", "value", "effect", "tolerationSeconds"];
const SPREAD_KEYS: &[&str] = &[
    "maxSkew",
    "topologyKey",
    "whenUnsatisfiable",
    "labelSelector",
    "minDomains",
    "nodeAffinityPolicy",
    "nodeTaintsPolicy",
    "matchLabelKeys",
];

fn resolve_placement_fields(
    table: &str,
    raw: &RawPlacementFields,
) -> Result<PlacementOverlay, Error> {
    let tolerations = raw
        .tolerations
        .as_ref()
        .map(|t| object_list(&format!("{table}.tolerations"), t, TOLERATION_KEYS, &[]))
        .transpose()?;
    let topology_spread = match &raw.topology_spread {
        None => None,
        Some(toml::Value::Boolean(true)) => Some(vec![default_spread()]),
        Some(toml::Value::Boolean(false)) => Some(Vec::new()),
        Some(toml::Value::Array(items)) => Some(object_list(
            &format!("{table}.topology_spread"),
            items,
            SPREAD_KEYS,
            &["maxSkew", "topologyKey", "whenUnsatisfiable"],
        )?),
        Some(_) => {
            return Err(invalid(
                format!("{table}.topology_spread"),
                "must be true, false, or a list of constraint tables",
            ));
        }
    };
    Ok(PlacementOverlay {
        node_selector: raw.node_selector.clone(),
        tolerations,
        topology_spread,
    })
}

struct PlacementOverlay {
    node_selector: Option<BTreeMap<String, String>>,
    tolerations: Option<Vec<toml::Value>>,
    topology_spread: Option<Vec<toml::Value>>,
}

impl PlacementOverlay {
    fn apply(self, spec: &mut PlacementSpec) {
        if let Some(v) = self.node_selector {
            spec.node_selector = v;
        }
        if let Some(v) = self.tolerations {
            spec.tolerations = v;
        }
        if let Some(v) = self.topology_spread {
            spec.topology_spread = v;
        }
    }
}

/// `[placement]` for `env`: each field of `[placement.<env>]` replaces the
/// base field. Every env table is validated, not only the selected one.
pub(crate) fn resolve_placement(
    raw: Option<&RawPlacement>,
    env: &str,
) -> Result<PlacementSpec, Error> {
    let mut spec = PlacementSpec::default();
    let Some(raw) = raw else {
        return Ok(spec);
    };
    resolve_placement_fields("[placement]", &raw.base)?.apply(&mut spec);
    for (e, fields) in &raw.envs {
        let overlay = resolve_placement_fields(&format!("[placement.{e}]"), fields)?;
        if e == env {
            overlay.apply(&mut spec);
        }
    }
    Ok(spec)
}

fn int_or_percent(field: &str, v: &toml::Value) -> Result<IntOrPercent, Error> {
    match v {
        toml::Value::Integer(n) if *n >= 0 => u32::try_from(*n)
            .map(IntOrPercent::Int)
            .map_err(|_| invalid(field, "out of range")),
        toml::Value::String(s)
            if s.strip_suffix('%')
                .is_some_and(|n| n.parse::<u8>().is_ok_and(|p| p <= 100)) =>
        {
            Ok(IntOrPercent::Percent(s.clone()))
        }
        _ => Err(invalid(
            field,
            "must be a non-negative integer or a percentage like \"50%\"",
        )),
    }
}

/// `(enabled, budget)` set by one `[pdb]` table.
fn resolve_pdb_fields(
    table: &str,
    raw: &RawPdbFields,
) -> Result<(Option<bool>, Option<PdbSpec>), Error> {
    let min = raw
        .min_available
        .as_ref()
        .map(|v| int_or_percent(&format!("{table}.min_available"), v))
        .transpose()?;
    let max = raw
        .max_unavailable
        .as_ref()
        .map(|v| int_or_percent(&format!("{table}.max_unavailable"), v))
        .transpose()?;
    if min.is_some() && max.is_some() {
        return Err(invalid(
            table,
            "set either min_available or max_unavailable, not both",
        ));
    }
    let budget = (min.is_some() || max.is_some()).then_some(PdbSpec {
        min_available: min,
        max_unavailable: max,
    });
    Ok((raw.enabled, budget))
}

/// `[pdb]` for `env`. Present means enabled (unless `enabled = false`), with
/// `max_unavailable = 1` when no budget is set. A budget in `[pdb.<env>]`
/// replaces the base one. `replicas` is the env's minimum replica count: a
/// `min_available` that would block every eviction is an error.
pub(crate) fn resolve_pdb(
    raw: Option<&RawPdb>,
    env: &str,
    replicas: u32,
) -> Result<Option<PdbSpec>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let (mut enabled, mut budget) = resolve_pdb_fields("[pdb]", &raw.base)?;
    for (e, fields) in &raw.envs {
        let (en, b) = resolve_pdb_fields(&format!("[pdb.{e}]"), fields)?;
        if e == env {
            enabled = en.or(enabled);
            budget = b.or(budget);
        }
    }
    if enabled == Some(false) {
        return Ok(None);
    }
    let budget = budget.unwrap_or(PdbSpec {
        min_available: None,
        max_unavailable: Some(IntOrPercent::Int(1)),
    });
    if replicas > 1
        && let Some(IntOrPercent::Int(n)) = budget.min_available
        && n >= replicas
    {
        return Err(invalid(
            "[pdb].min_available",
            format!(
                "{n} with {replicas} replicas in env {env:?} blocks every voluntary eviction \
                 (node drains hang); use a value below the replica count or max_unavailable"
            ),
        ));
    }
    if budget.max_unavailable == Some(IntOrPercent::Int(0)) {
        return Err(invalid(
            "[pdb].max_unavailable",
            "0 blocks every voluntary eviction (node drains hang)",
        ));
    }
    Ok(Some(budget))
}

/// Default path of a projected token: `/var/run/secrets/tokens/<audience>`.
pub const DEFAULT_TOKEN_DIR: &str = "/var/run/secrets/tokens";
/// Kubernetes rejects projected tokens valid for less than 10 minutes.
const MIN_TOKEN_EXPIRATION: u32 = 600;
const DEFAULT_TOKEN_EXPIRATION: u32 = 3600;

pub(crate) fn resolve_service_account(
    raw: Option<&RawServiceAccount>,
) -> Result<ServiceAccountSpec, Error> {
    let Some(raw) = raw else {
        return Ok(ServiceAccountSpec::default());
    };
    if raw.token.is_some() && !raw.tokens.is_empty() {
        return Err(invalid(
            "[service_account]",
            "set either `token` or `tokens`, not both",
        ));
    }
    let mut tokens = Vec::new();
    let mut paths = BTreeSet::new();
    for (i, t) in raw.token.iter().chain(&raw.tokens).enumerate() {
        let field = format!("[service_account].tokens[{i}]");
        if t.audience.trim().is_empty() {
            return Err(invalid(format!("{field}.audience"), "must not be empty"));
        }
        let path = t
            .path
            .clone()
            .unwrap_or_else(|| format!("{DEFAULT_TOKEN_DIR}/{}", t.audience));
        let (_, file) = split_path(&path);
        if !path.starts_with('/')
            || file.is_empty()
            || path.split('/').any(|seg| seg == ".." || seg == ".")
        {
            return Err(invalid(
                format!("{field}.path"),
                format!("{path:?} must be an absolute file path without `.`/`..` segments"),
            ));
        }
        if !paths.insert(path.clone()) {
            return Err(invalid(
                format!("{field}.path"),
                format!("{path:?} is used by another token"),
            ));
        }
        let expiration_seconds = t.expiration_seconds.unwrap_or(DEFAULT_TOKEN_EXPIRATION);
        if expiration_seconds < MIN_TOKEN_EXPIRATION {
            return Err(invalid(
                format!("{field}.expiration_seconds"),
                format!("must be at least {MIN_TOKEN_EXPIRATION} (Kubernetes minimum)"),
            ));
        }
        tokens.push(SaToken {
            audience: t.audience.clone(),
            path,
            expiration_seconds,
        });
    }
    Ok(ServiceAccountSpec {
        automount_token: raw.automount_token.unwrap_or(false),
        tokens,
    })
}

/// A DNS-1123 label: lowercase alphanumerics and `-`, alphanumeric at both ends.
fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// CronJob names are limited to 52 characters (the controller appends an
/// 11-character suffix to Job names).
const MAX_CRONJOB_NAME: usize = 52;

fn concurrency_policy(field: &str, v: Option<&str>) -> Result<String, Error> {
    match v.map(str::to_ascii_lowercase).as_deref() {
        None | Some("forbid") => Ok("Forbid".into()),
        Some("allow") => Ok("Allow".into()),
        Some("replace") => Ok("Replace".into()),
        Some(_) => Err(invalid(
            field,
            "must be \"Allow\", \"Forbid\" or \"Replace\"",
        )),
    }
}

fn valid_schedule(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with('@') && s.len() > 1) || s.split_whitespace().count() == 5
}

/// `[[cronjobs]]` for `env`. Every entry is validated, also those filtered
/// out by `envs`.
pub(crate) fn resolve_cronjobs(
    raw: &[RawCronJob],
    service: &str,
    env: &str,
) -> Result<Vec<CronJobSpec>, Error> {
    let mut names = BTreeSet::new();
    let mut out = Vec::new();
    for (i, c) in raw.iter().enumerate() {
        let field = format!("[[cronjobs]][{i}]");
        if !is_dns_label(&c.name) {
            return Err(invalid(
                format!("{field}.name"),
                format!(
                    "{:?} must be lowercase alphanumerics and '-' (a DNS label)",
                    c.name
                ),
            ));
        }
        let full = format!("{service}-{}", c.name);
        if full.len() > MAX_CRONJOB_NAME {
            return Err(invalid(
                format!("{field}.name"),
                format!("CronJob name {full:?} exceeds {MAX_CRONJOB_NAME} characters"),
            ));
        }
        if !names.insert(c.name.clone()) {
            return Err(invalid(
                format!("{field}.name"),
                format!("{:?} is declared twice", c.name),
            ));
        }
        if !valid_schedule(&c.schedule) {
            return Err(invalid(
                format!("{field}.schedule"),
                format!(
                    "{:?} is not a cron schedule (5 fields, or a macro like @hourly)",
                    c.schedule
                ),
            ));
        }
        if c.command.as_ref().is_none_or(Vec::is_empty) && c.args.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(invalid(
                field,
                "set `command` and/or `args`; without them the job would run the server",
            ));
        }
        let policy = concurrency_policy(
            &format!("{field}.concurrency_policy"),
            c.concurrency_policy.as_deref(),
        )?;
        if let Some(envs) = &c.envs
            && !envs.iter().any(|e| e == env)
        {
            continue;
        }
        out.push(CronJobSpec {
            name: c.name.clone(),
            schedule: c.schedule.trim().to_string(),
            time_zone: c.time_zone.clone(),
            command: c.command.clone().filter(|v| !v.is_empty()),
            args: c.args.clone().unwrap_or_default(),
            concurrency_policy: policy,
            successful_jobs_history_limit: c.successful_jobs_history_limit.unwrap_or(3),
            failed_jobs_history_limit: c.failed_jobs_history_limit.unwrap_or(3),
            starting_deadline_seconds: c.starting_deadline_seconds,
            active_deadline_seconds: c.active_deadline_seconds,
            backoff_limit: c.backoff_limit.unwrap_or(3),
            suspend: c.suspend.unwrap_or(false),
            resources: c.resources.as_ref().map(|r| CronResources {
                requests: CpuMemory {
                    cpu: r.cpu.clone(),
                    memory: r.memory.clone(),
                },
                limits: CpuMemory {
                    cpu: r.cpu.clone(),
                    memory: r.memory.clone(),
                },
            }),
            env: c.env.clone(),
        });
    }
    Ok(out)
}

/// Secret names for `imagePullSecrets` (DNS-1123 subdomains).
pub(crate) fn validate_pull_secrets(table: &str, names: &[String]) -> Result<(), Error> {
    for n in names {
        let ok = !n.is_empty() && n.len() <= 253 && n.split('.').all(is_dns_label);
        if !ok {
            return Err(invalid(
                format!("{table}.image_pull_secrets"),
                format!("{n:?} is not a valid Secret name"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T: serde::de::DeserializeOwned>(s: &str) -> T {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn placement_defaults_to_hostname_spread_only() {
        let spec = resolve_placement(None, "prd").unwrap();
        assert!(spec.node_selector.is_empty());
        assert!(spec.tolerations.is_empty());
        assert_eq!(spec.topology_spread, vec![default_spread()]);
    }

    #[test]
    fn placement_env_overlay_replaces_fields() {
        let raw: RawPlacement = parse(
            r#"
node_selector = { "example.com/role" = "app" }

[prd]
tolerations = [{ key = "example.com/db", operator = "Equal", value = "identity", effect = "NoSchedule" }]
topology_spread = false

[dev]
node_selector = {}
"#,
        );
        let prd = resolve_placement(Some(&raw), "prd").unwrap();
        assert_eq!(prd.node_selector["example.com/role"], "app");
        assert_eq!(prd.tolerations.len(), 1);
        assert_eq!(prd.tolerations[0]["key"].as_str(), Some("example.com/db"));
        assert!(prd.topology_spread.is_empty());

        let dev = resolve_placement(Some(&raw), "dev").unwrap();
        assert!(dev.node_selector.is_empty());
        assert!(dev.tolerations.is_empty());
        assert_eq!(dev.topology_spread.len(), 1);
    }

    #[test]
    fn placement_snake_case_keys_become_camel_case() {
        let raw: RawPlacement = parse(
            r#"
tolerations = [{ key = "k", operator = "Exists", effect = "NoExecute", toleration_seconds = 30 }]
topology_spread = [{ max_skew = 1, topology_key = "topology.kubernetes.io/zone", when_unsatisfiable = "DoNotSchedule" }]
"#,
        );
        let spec = resolve_placement(Some(&raw), "prd").unwrap();
        assert_eq!(
            spec.tolerations[0]["tolerationSeconds"].as_integer(),
            Some(30)
        );
        assert_eq!(
            spec.topology_spread[0]["topologyKey"].as_str(),
            Some("topology.kubernetes.io/zone")
        );
    }

    #[test]
    fn placement_rejects_unknown_keys_in_every_env() {
        let raw: RawPlacement = parse(
            r#"
[stg]
tolerations = [{ key = "k", efect = "NoSchedule" }]
"#,
        );
        let err = resolve_placement(Some(&raw), "prd")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("[placement.stg].tolerations[0].efect"),
            "{err}"
        );

        let raw: RawPlacement = parse("topology_spread = [{ max_skew = 1 }]");
        let err = resolve_placement(Some(&raw), "prd")
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing `topologyKey`"), "{err}");

        let raw: RawPlacement = parse("topology_spread = \"hostname\"");
        assert!(resolve_placement(Some(&raw), "prd").is_err());
    }

    #[test]
    fn pdb_absent_is_none_and_present_defaults_to_max_unavailable_one() {
        assert_eq!(resolve_pdb(None, "prd", 3).unwrap(), None);
        let raw: RawPdb = parse("");
        assert_eq!(
            resolve_pdb(Some(&raw), "prd", 3).unwrap(),
            Some(PdbSpec {
                min_available: None,
                max_unavailable: Some(IntOrPercent::Int(1)),
            })
        );
    }

    #[test]
    fn pdb_env_overlay_replaces_budget_and_can_disable() {
        let raw: RawPdb = parse(
            r#"
max_unavailable = "50%"

[prd]
min_available = 1

[dev]
enabled = false
"#,
        );
        assert_eq!(
            resolve_pdb(Some(&raw), "prd", 2).unwrap(),
            Some(PdbSpec {
                min_available: Some(IntOrPercent::Int(1)),
                max_unavailable: None,
            })
        );
        assert_eq!(resolve_pdb(Some(&raw), "dev", 2).unwrap(), None);
        assert_eq!(
            resolve_pdb(Some(&raw), "stg", 2)
                .unwrap()
                .unwrap()
                .max_unavailable,
            Some(IntOrPercent::Percent("50%".into()))
        );
    }

    #[test]
    fn pdb_rejects_budgets_that_block_evictions_and_bad_values() {
        let raw: RawPdb = parse("min_available = 2");
        let err = resolve_pdb(Some(&raw), "prd", 2).unwrap_err().to_string();
        assert!(err.contains("blocks every voluntary eviction"), "{err}");
        // With one replica the PDB is not rendered, so it is not an error.
        assert!(resolve_pdb(Some(&raw), "dev", 1).is_ok());

        let raw: RawPdb = parse("max_unavailable = 0");
        assert!(resolve_pdb(Some(&raw), "prd", 2).is_err());

        let raw: RawPdb = parse("min_available = 1\nmax_unavailable = 1");
        assert!(resolve_pdb(Some(&raw), "prd", 3).is_err());

        let raw: RawPdb = parse("[prd]\nmax_unavailable = \"150%\"");
        let err = resolve_pdb(Some(&raw), "dev", 3).unwrap_err().to_string();
        assert!(err.contains("[pdb.prd].max_unavailable"), "{err}");
    }

    #[test]
    fn service_account_defaults_to_no_automount_and_no_tokens() {
        let spec = resolve_service_account(None).unwrap();
        assert!(!spec.automount_token);
        assert!(spec.tokens.is_empty());
        assert!(spec.token_volumes().is_empty());
    }

    #[test]
    fn service_account_tokens_group_by_directory() {
        let raw: RawServiceAccount = parse(
            r#"
tokens = [
  { audience = "identity" },
  { audience = "telemetry", expiration_seconds = 7200 },
  { audience = "vault", path = "/etc/vault/token" },
]
"#,
        );
        let spec = resolve_service_account(Some(&raw)).unwrap();
        assert_eq!(spec.tokens[0].path, "/var/run/secrets/tokens/identity");
        let vols = spec.token_volumes();
        assert_eq!(vols.len(), 2);
        assert_eq!(vols[0].name, "sa-token-0");
        assert_eq!(vols[0].mount_path, "/etc/vault");
        assert_eq!(vols[1].mount_path, "/var/run/secrets/tokens");
        assert_eq!(
            vols[1].sources,
            vec![
                TokenSource {
                    audience: "identity".into(),
                    expiration_seconds: 3600,
                    path: "identity".into(),
                },
                TokenSource {
                    audience: "telemetry".into(),
                    expiration_seconds: 7200,
                    path: "telemetry".into(),
                },
            ]
        );
    }

    #[test]
    fn service_account_token_shorthand_and_validation() {
        let raw: RawServiceAccount =
            parse(r#"token = { audience = "identity", path = "/var/run/secrets/tokens/id" }"#);
        let spec = resolve_service_account(Some(&raw)).unwrap();
        assert_eq!(spec.tokens.len(), 1);

        for bad in [
            r#"token = { audience = "a" }
tokens = [{ audience = "b" }]"#,
            r#"tokens = [{ audience = "a", path = "relative/token" }]"#,
            r#"tokens = [{ audience = "a", path = "/var/run/../token" }]"#,
            r#"tokens = [{ audience = "a", path = "/dir/" }]"#,
            r#"tokens = [{ audience = "a", expiration_seconds = 60 }]"#,
            r#"tokens = [{ audience = "a" }, { audience = "a" }]"#,
            r#"tokens = [{ audience = "" }]"#,
        ] {
            let raw: RawServiceAccount = parse(bad);
            assert!(resolve_service_account(Some(&raw)).is_err(), "{bad}");
        }
        assert!(toml::from_str::<RawServiceAccount>("automount = true").is_err());
    }

    #[derive(Deserialize)]
    struct Crons {
        cronjobs: Vec<RawCronJob>,
    }

    fn crons(s: &str) -> Vec<RawCronJob> {
        parse::<Crons>(s).cronjobs
    }

    #[test]
    fn cronjobs_resolve_with_defaults_and_env_filter() {
        let raw = crons(
            r#"
[[cronjobs]]
name     = "purge-expired"
schedule = "0 2 * * *"
command  = ["/usr/local/bin/identity-purge-expired"]

[[cronjobs]]
name               = "report"
schedule           = "@hourly"
args               = ["report", "--all"]
concurrency_policy = "replace"
resources          = { cpu = "50m", memory = "64Mi" }
env                = { REPORT_MODE = "full" }
envs               = ["prd"]
"#,
        );
        let dev = resolve_cronjobs(&raw, "identity", "dev").unwrap();
        assert_eq!(dev.len(), 1);
        let purge = &dev[0];
        assert_eq!(purge.concurrency_policy, "Forbid");
        assert_eq!(purge.successful_jobs_history_limit, 3);
        assert_eq!(purge.failed_jobs_history_limit, 3);
        assert_eq!(purge.backoff_limit, 3);
        assert!(purge.args.is_empty());

        let prd = resolve_cronjobs(&raw, "identity", "prd").unwrap();
        assert_eq!(prd.len(), 2);
        assert_eq!(prd[1].concurrency_policy, "Replace");
        assert_eq!(prd[1].command, None);
        assert_eq!(prd[1].args, vec!["report", "--all"]);
        assert_eq!(prd[1].resources.as_ref().unwrap().limits.memory, "64Mi");
    }

    #[test]
    fn cronjobs_reject_invalid_entries_even_when_filtered_out() {
        for bad in [
            "name = \"Purge\"\nschedule = \"0 2 * * *\"\nargs = [\"x\"]",
            "name = \"p\"\nschedule = \"0 2 * *\"\nargs = [\"x\"]",
            "name = \"p\"\nschedule = \"0 2 * * *\"",
            "name = \"p\"\nschedule = \"0 2 * * *\"\nargs = [\"x\"]\nconcurrency_policy = \"never\"",
            "name = \"a-very-long-cronjob-name-that-is-too-long-for-k8s\"\nschedule = \"@daily\"\nargs = [\"x\"]",
        ] {
            let raw = crons(&format!("[[cronjobs]]\n{bad}\nenvs = [\"stg\"]"));
            assert!(resolve_cronjobs(&raw, "identity", "prd").is_err(), "{bad}");
        }
        let dup = crons(
            "[[cronjobs]]\nname = \"p\"\nschedule = \"@daily\"\nargs = [\"x\"]\n\
             [[cronjobs]]\nname = \"p\"\nschedule = \"@daily\"\nargs = [\"y\"]",
        );
        assert!(resolve_cronjobs(&dup, "identity", "prd").is_err());
        assert!(
            toml::from_str::<Crons>("[[cronjobs]]\nname = \"p\"\nschedul = \"@daily\"").is_err()
        );
    }

    #[test]
    fn pull_secret_names_are_validated() {
        assert!(validate_pull_secrets("[deploy]", &["ghcr-read".into()]).is_ok());
        assert!(validate_pull_secrets("[deploy]", &["GHCR".into()]).is_err());
        assert!(validate_pull_secrets("[deploy]", &[String::new()]).is_err());
    }

    /// A full `tonin.toml` loaded for `env` (plan-level resolution).
    fn load(extra: &str, env: &str) -> Result<crate::Plan, Error> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tonin.toml");
        std::fs::write(
            &path,
            format!(
                "[service]\nname = \"identity\"\nversion = \"0.1.0\"\n\
                 [resources]\ncpu = \"100m\"\nmemory = \"128Mi\"\n{extra}"
            ),
        )
        .unwrap();
        crate::Plan::load_with_env(&path, env)
    }

    #[test]
    fn plan_resolves_pull_secrets_per_env() {
        let toml = r#"
[deploy]
replicas  = 2
namespace = "platform"
image_pull_secrets = ["base-read"]

[deploy.prd]
image_pull_secrets = ["ghcr-read"]

[deploy.dev]
image_pull_secrets = []
"#;
        assert_eq!(
            load(toml, "prd").unwrap().pod.image_pull_secrets,
            ["ghcr-read"]
        );
        assert!(load(toml, "dev").unwrap().pod.image_pull_secrets.is_empty());
        assert_eq!(
            load(toml, "stg").unwrap().pod.image_pull_secrets,
            ["base-read"]
        );

        let bad = "[deploy]\nreplicas = 1\nnamespace = \"p\"\n[deploy.prd]\nimage_pull_secrets = [\"Bad_Name\"]";
        let err = load(bad, "dev").unwrap_err().to_string();
        assert!(err.contains("[deploy.prd].image_pull_secrets"), "{err}");
    }

    #[test]
    fn plan_without_pod_sections_keeps_defaults() {
        let plan = load("[deploy]\nreplicas = 1\nnamespace = \"p\"", "prd").unwrap();
        assert!(plan.pod.image_pull_secrets.is_empty());
        assert_eq!(plan.pod.placement, PlacementSpec::default());
        assert_eq!(plan.pod.pdb, None);
        assert_eq!(plan.pod.service_account, ServiceAccountSpec::default());
        assert!(plan.pod.cronjobs.is_empty());
    }

    #[test]
    fn plan_pdb_uses_the_env_replica_count() {
        let toml = r#"
[deploy]
replicas  = 3
namespace = "p"

[deploy.dev]
replicas = 2

[pdb]
min_available = 2
"#;
        assert!(load(toml, "prd").unwrap().pod.pdb.is_some());
        assert!(load(toml, "dev").is_err());
    }

    #[test]
    fn plan_rejects_cronjob_env_shadowing_a_tonin_env_var() {
        let toml = r#"
[deploy]
replicas  = 1
namespace = "p"

[database]
engine = "postgres"

[[cronjobs]]
name     = "purge"
schedule = "@daily"
args     = ["purge"]
env      = { DATABASE_URL = "postgres://elsewhere" }
"#;
        let err = load(toml, "prd").unwrap_err().to_string();
        assert!(err.contains("purge.env.DATABASE_URL"), "{err}");
    }
}
