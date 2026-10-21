//! Workload shape: `[deploy] kind` (Deployment or StatefulSet), update
//! strategy, `[[volumes]]`, `[[ports]]`, probes, `[resources]`, the container
//! command and `[autoscale]`.
//!
//! Everything here resolves into a [`WorkloadSpec`] on the [`crate::Plan`],
//! which the Helm generator renders into the chart's values.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::plan::Error;

// ---------- on-disk TOML shape ----------

/// `[deploy.strategy]` / `[deploy.<env>.strategy]`.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawStrategy {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// StatefulSet RollingUpdate: pods with an ordinal >= partition are updated.
    #[serde(default)]
    pub partition: Option<u32>,
    /// Deployment RollingUpdate: an integer or a percentage string.
    #[serde(default)]
    pub max_surge: Option<toml::Value>,
    /// Deployment RollingUpdate: an integer or a percentage string.
    #[serde(default)]
    pub max_unavailable: Option<toml::Value>,
}

impl RawStrategy {
    fn merge(base: Option<&Self>, overlay: Option<&Self>) -> Option<Self> {
        match (base, overlay) {
            (None, None) => None,
            (b, o) => {
                let b = b.cloned().unwrap_or_default();
                let o = o.cloned().unwrap_or_default();
                Some(Self {
                    kind: o.kind.or(b.kind),
                    partition: o.partition.or(b.partition),
                    max_surge: o.max_surge.or(b.max_surge),
                    max_unavailable: o.max_unavailable.or(b.max_unavailable),
                })
            }
        }
    }
}

/// One `[[volumes]]` entry.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawVolume {
    pub name: String,
    /// `"empty-dir"` or `"pvc"`.
    #[serde(rename = "type")]
    pub kind: String,
    pub mount_path: String,
    #[serde(default)]
    pub read_only: Option<bool>,
    /// empty-dir: `sizeLimit`.
    #[serde(default)]
    pub size_limit: Option<String>,
    /// empty-dir: `"Memory"` for a tmpfs.
    #[serde(default)]
    pub medium: Option<String>,
    /// pvc: storage request.
    #[serde(default)]
    pub size: Option<String>,
    /// pvc: StorageClass; omitted = the cluster's default class.
    #[serde(default)]
    pub storage_class: Option<String>,
    /// pvc: access modes (default `["ReadWriteOnce"]`).
    #[serde(default)]
    pub access_modes: Option<Vec<String>>,
}

/// One `[[ports]]` entry.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPort {
    pub name: String,
    pub port: u32,
    #[serde(default)]
    pub protocol: Option<String>,
    /// Put the port on an extra Service `<service>-<suffix>` instead of the
    /// main one.
    #[serde(default)]
    pub service: Option<String>,
}

/// `[service.health.startup]`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawStartupProbe {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub period_seconds: Option<u32>,
    #[serde(default)]
    pub failure_threshold: Option<u32>,
}

/// `[resources.requests]` / `[resources.limits]`.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawResourceList {
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub ephemeral_storage: Option<String>,
}

/// `[resources]`: either the `cpu`/`memory` shorthand (requests = limits) or
/// explicit `[resources.requests]` / `[resources.limits]` tables.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawResources {
    #[serde(default)]
    pub cpu: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub requests: Option<RawResourceList>,
    #[serde(default)]
    pub limits: Option<RawResourceList>,
}

/// Fields of `[autoscale]` and `[autoscale.<env>]`.
#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct RawAutoscaleFields {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub min_replicas: Option<u32>,
    #[serde(default)]
    pub max_replicas: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct RawAutoscale {
    #[serde(flatten)]
    pub base: RawAutoscaleFields,
    #[serde(flatten)]
    pub envs: BTreeMap<String, RawAutoscaleFields>,
}

// ---------- resolved types ----------

/// Kubernetes workload kind of the service (`[deploy] kind`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum WorkloadKind {
    #[default]
    Deployment,
    StatefulSet,
}

impl WorkloadKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Deployment => "Deployment",
            Self::StatefulSet => "StatefulSet",
        }
    }
}

/// A resolved `[[volumes]]` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeSpec {
    pub name: String,
    pub mount_path: String,
    pub read_only: bool,
    pub source: VolumeSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeSource {
    /// A pod-lifetime `emptyDir`.
    EmptyDir {
        size_limit: Option<String>,
        medium: Option<String>,
    },
    /// A StatefulSet `volumeClaimTemplates` entry (one PVC per pod).
    Pvc {
        size: String,
        storage_class: Option<String>,
        access_modes: Vec<String>,
    },
}

/// A resolved `[[ports]]` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortSpec {
    pub name: String,
    pub port: u32,
    pub protocol: String,
    /// Extra Service suffix; empty = the main Service.
    pub service: String,
}

/// `[service.health.startup]`, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupProbeSpec {
    /// httpGet path (ignored for gRPC probes).
    pub path: String,
    pub period_seconds: u32,
    pub failure_threshold: u32,
}

/// Probe paths and the optional startupProbe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbesSpec {
    /// httpGet path of the livenessProbe (empty for gRPC probes).
    pub liveness_path: String,
    /// httpGet path of the readinessProbe (empty for gRPC probes).
    pub readiness_path: String,
    pub startup: Option<StartupProbeSpec>,
}

/// Container resources: `cpu`, `memory`, `ephemeral-storage` per list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourcesSpec {
    pub requests: BTreeMap<String, String>,
    pub limits: BTreeMap<String, String>,
}

/// HorizontalPodAutoscaler range for the selected env.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoscaleSpec {
    pub min_replicas: u32,
    pub max_replicas: u32,
}

/// Update strategy, already in Kubernetes shape (`strategy` on a Deployment,
/// `updateStrategy` on a StatefulSet).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StrategySpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "rollingUpdate", skip_serializing_if = "BTreeMap::is_empty")]
    pub rolling_update: BTreeMap<String, serde_yaml_value::Value>,
}

/// Minimal YAML-ish scalar for strategy fields (int or string), kept local so
/// tonin-plugin does not depend on serde_yaml.
pub mod serde_yaml_value {
    use serde::Serialize;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(untagged)]
    pub enum Value {
        Int(i64),
        Str(String),
    }
}

/// Everything about the pod and its controller that is not a datastore,
/// secret or policy, resolved for one environment.
#[derive(Clone, Debug, Default)]
pub struct WorkloadSpec {
    pub kind: WorkloadKind,
    /// StatefulSet `podManagementPolicy` (`OrderedReady` | `Parallel`).
    pub pod_management_policy: Option<String>,
    pub strategy: Option<StrategySpec>,
    pub termination_grace_period_seconds: Option<u32>,
    /// Sorted by mount path, so a parent directory is mounted before a
    /// volume nested inside it.
    pub volumes: Vec<VolumeSpec>,
    pub ports: Vec<PortSpec>,
    pub probes: ProbesSpec,
    pub resources: ResourcesSpec,
    /// Container command; empty = the image's ENTRYPOINT/CMD.
    pub command: Vec<String>,
    pub args: Vec<String>,
    /// `None` = no HorizontalPodAutoscaler in this env.
    pub autoscale: Option<AutoscaleSpec>,
}

impl WorkloadSpec {
    /// Suffixes of the extra Services (`<service>-<suffix>`), sorted.
    pub fn extra_services(&self) -> Vec<String> {
        self.ports
            .iter()
            .filter(|p| !p.service.is_empty())
            .map(|p| p.service.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

// ---------- validation helpers ----------

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Error {
    Error::InvalidField {
        field: field.into(),
        reason: reason.into(),
    }
}

/// RFC 1123 label: lowercase alphanumerics and '-', starting and ending
/// alphanumeric, at most `max` characters.
fn is_dns_label(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// IANA service name (a container port name): at most 15 characters,
/// lowercase alphanumerics and '-', at least one letter, no "--".
fn is_port_name(s: &str) -> bool {
    is_dns_label(s, 15) && s.bytes().any(|b| b.is_ascii_lowercase()) && !s.contains("--")
}

/// A Kubernetes resource quantity (`100m`, `1.5Gi`, `8Gi`, `512M`, `2`).
pub(crate) fn is_quantity(s: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "Ki", "Mi", "Gi", "Ti", "Pi", "Ei", "m", "k", "M", "G", "T", "P", "E", "",
    ];
    SUFFIXES.iter().any(|suffix| {
        s.strip_suffix(suffix).is_some_and(|num| {
            let mut parts = num.splitn(2, '.');
            let int = parts.next().unwrap_or("");
            let frac = parts.next();
            !int.is_empty()
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
        })
    })
}

fn quantity(field: &str, value: &str) -> Result<String, Error> {
    if is_quantity(value) {
        Ok(value.to_string())
    } else {
        Err(invalid(
            field,
            format!("{value:?} is not a Kubernetes quantity (e.g. \"500m\", \"256Mi\", \"8Gi\")"),
        ))
    }
}

// ---------- resolution ----------

pub(crate) fn parse_kind(raw: Option<&str>) -> Result<WorkloadKind, Error> {
    match raw {
        None | Some("deployment") => Ok(WorkloadKind::Deployment),
        Some("statefulset") => Ok(WorkloadKind::StatefulSet),
        Some(other) => Err(invalid(
            "[deploy].kind",
            format!("{other:?} is not supported; use \"deployment\" or \"statefulset\""),
        )),
    }
}

pub(crate) fn resolve_pod_management_policy(
    kind: WorkloadKind,
    raw: Option<&str>,
) -> Result<Option<String>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if kind != WorkloadKind::StatefulSet {
        return Err(invalid(
            "[deploy].pod_management_policy",
            "only applies to kind = \"statefulset\"",
        ));
    }
    match raw {
        "OrderedReady" | "Parallel" => Ok(Some(raw.to_string())),
        other => Err(invalid(
            "[deploy].pod_management_policy",
            format!("{other:?} is not supported; use \"OrderedReady\" or \"Parallel\""),
        )),
    }
}

fn int_or_percent(field: &str, v: &toml::Value) -> Result<serde_yaml_value::Value, Error> {
    match v {
        toml::Value::Integer(i) if *i >= 0 => Ok(serde_yaml_value::Value::Int(*i)),
        toml::Value::String(s)
            if s.strip_suffix('%')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            Ok(serde_yaml_value::Value::Str(s.clone()))
        }
        other => Err(invalid(
            field,
            format!("must be a non-negative integer or a percentage like \"25%\", got {other}"),
        )),
    }
}

pub(crate) fn resolve_strategy(
    kind: WorkloadKind,
    base: Option<&RawStrategy>,
    overlay: Option<&RawStrategy>,
    env: &str,
) -> Result<Option<StrategySpec>, Error> {
    let Some(s) = RawStrategy::merge(base, overlay) else {
        return Ok(None);
    };
    let table = if overlay.is_some() {
        format!("[deploy.{env}.strategy]")
    } else {
        "[deploy.strategy]".to_string()
    };
    let ty = s.kind.as_deref().unwrap_or("RollingUpdate");
    let mut rolling = BTreeMap::new();
    match kind {
        WorkloadKind::Deployment => {
            if !matches!(ty, "RollingUpdate" | "Recreate") {
                return Err(invalid(
                    format!("{table}.type"),
                    format!(
                        "{ty:?} is not a Deployment strategy; use \"RollingUpdate\" or \"Recreate\""
                    ),
                ));
            }
            if s.partition.is_some() {
                return Err(invalid(
                    format!("{table}.partition"),
                    "only applies to kind = \"statefulset\"",
                ));
            }
            for (key, val) in [
                ("maxSurge", &s.max_surge),
                ("maxUnavailable", &s.max_unavailable),
            ] {
                if let Some(v) = val {
                    if ty != "RollingUpdate" {
                        return Err(invalid(
                            table.clone(),
                            format!("{key} only applies to type = \"RollingUpdate\""),
                        ));
                    }
                    rolling.insert(
                        key.to_string(),
                        int_or_percent(&format!("{table}.{key}"), v)?,
                    );
                }
            }
        }
        WorkloadKind::StatefulSet => {
            if !matches!(ty, "RollingUpdate" | "OnDelete") {
                return Err(invalid(
                    format!("{table}.type"),
                    format!(
                        "{ty:?} is not a StatefulSet update strategy; use \"RollingUpdate\" or \"OnDelete\""
                    ),
                ));
            }
            if s.max_surge.is_some() || s.max_unavailable.is_some() {
                return Err(invalid(
                    table.clone(),
                    "max_surge / max_unavailable only apply to kind = \"deployment\" \
                     (a StatefulSet rolls one pod at a time; use `partition` to stage a rollout)",
                ));
            }
            if let Some(p) = s.partition {
                if ty != "RollingUpdate" {
                    return Err(invalid(
                        format!("{table}.partition"),
                        "only applies to type = \"RollingUpdate\"",
                    ));
                }
                rolling.insert(
                    "partition".to_string(),
                    serde_yaml_value::Value::Int(i64::from(p)),
                );
            }
        }
    }
    Ok(Some(StrategySpec {
        kind: ty.to_string(),
        rolling_update: rolling,
    }))
}

/// Volume names tonin itself uses in the pod (the Postgres CA mounts).
const RESERVED_VOLUME_PREFIX: &str = "pg-ca";

pub(crate) fn resolve_volumes(
    kind: WorkloadKind,
    raw: &[RawVolume],
) -> Result<Vec<VolumeSpec>, Error> {
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for (i, v) in raw.iter().enumerate() {
        let at = format!("[[volumes]] #{} ({:?})", i + 1, v.name);
        if !is_dns_label(&v.name, 63) {
            return Err(invalid(
                format!("{at}.name"),
                "must be a DNS-1123 label (lowercase letters, digits, '-')",
            ));
        }
        if v.name.starts_with(RESERVED_VOLUME_PREFIX) {
            return Err(invalid(
                format!("{at}.name"),
                format!(
                    "names starting with {RESERVED_VOLUME_PREFIX:?} are reserved for the Postgres CA mount"
                ),
            ));
        }
        if !names.insert(v.name.clone()) {
            return Err(invalid(format!("{at}.name"), "is declared twice"));
        }
        if !v.mount_path.starts_with('/') {
            return Err(invalid(
                format!("{at}.mount_path"),
                format!("{:?} must be an absolute path", v.mount_path),
            ));
        }
        let mount_path = v.mount_path.trim_end_matches('/').to_string();
        let mount_path = if mount_path.is_empty() {
            "/".to_string()
        } else {
            mount_path
        };
        if !paths.insert(mount_path.clone()) {
            return Err(invalid(
                format!("{at}.mount_path"),
                format!("{mount_path:?} is mounted twice"),
            ));
        }
        let only = |field: &str, set: bool, ty: &str| -> Result<(), Error> {
            if set {
                Err(invalid(
                    format!("{at}.{field}"),
                    format!("only applies to type = {ty:?}"),
                ))
            } else {
                Ok(())
            }
        };
        let source = match v.kind.as_str() {
            "empty-dir" => {
                only("size", v.size.is_some(), "pvc")?;
                only("storage_class", v.storage_class.is_some(), "pvc")?;
                only("access_modes", v.access_modes.is_some(), "pvc")?;
                let medium = match v.medium.as_deref() {
                    None | Some("") => None,
                    Some("Memory") => Some("Memory".to_string()),
                    Some(other) => {
                        return Err(invalid(
                            format!("{at}.medium"),
                            format!(
                                "{other:?} is not supported; use \"Memory\" or omit it (node disk)"
                            ),
                        ));
                    }
                };
                VolumeSource::EmptyDir {
                    size_limit: v
                        .size_limit
                        .as_deref()
                        .map(|s| quantity(&format!("{at}.size_limit"), s))
                        .transpose()?,
                    medium,
                }
            }
            "pvc" => {
                only("size_limit", v.size_limit.is_some(), "empty-dir")?;
                only("medium", v.medium.is_some(), "empty-dir")?;
                if kind != WorkloadKind::StatefulSet {
                    return Err(invalid(
                        format!("{at}.type"),
                        "a \"pvc\" volume needs [deploy] kind = \"statefulset\" (one claim per \
                         pod via volumeClaimTemplates); use \"empty-dir\" for scratch space",
                    ));
                }
                let size = v.size.as_deref().ok_or_else(|| {
                    invalid(
                        format!("{at}.size"),
                        "required for a pvc (e.g. size = \"8Gi\")",
                    )
                })?;
                let access_modes = v
                    .access_modes
                    .clone()
                    .unwrap_or_else(|| vec!["ReadWriteOnce".to_string()]);
                for m in &access_modes {
                    if !matches!(
                        m.as_str(),
                        "ReadWriteOnce" | "ReadOnlyMany" | "ReadWriteMany" | "ReadWriteOncePod"
                    ) {
                        return Err(invalid(
                            format!("{at}.access_modes"),
                            format!("{m:?} is not a PersistentVolume access mode"),
                        ));
                    }
                }
                if access_modes.is_empty() {
                    return Err(invalid(format!("{at}.access_modes"), "must not be empty"));
                }
                VolumeSource::Pvc {
                    size: quantity(&format!("{at}.size"), size)?,
                    storage_class: v.storage_class.clone(),
                    access_modes,
                }
            }
            other => {
                return Err(invalid(
                    format!("{at}.type"),
                    format!("{other:?} is not supported; use \"empty-dir\" or \"pvc\""),
                ));
            }
        };
        out.push(VolumeSpec {
            name: v.name.clone(),
            mount_path,
            read_only: v.read_only.unwrap_or(false),
            source,
        });
    }
    out.sort_by(|a, b| a.mount_path.cmp(&b.mount_path));
    Ok(out)
}

/// Ports and names tonin already uses in the pod; `[[ports]]` must not reuse them.
pub(crate) struct ReservedPorts {
    pub names: Vec<&'static str>,
    pub numbers: Vec<(u32, &'static str)>,
}

pub(crate) fn resolve_ports(
    service: &str,
    raw: &[RawPort],
    reserved: &ReservedPorts,
) -> Result<Vec<PortSpec>, Error> {
    let mut names = BTreeSet::new();
    let mut numbers = BTreeSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for (i, p) in raw.iter().enumerate() {
        let at = format!("[[ports]] #{} ({:?})", i + 1, p.name);
        if !is_port_name(&p.name) {
            return Err(invalid(
                format!("{at}.name"),
                "must be at most 15 characters of lowercase letters, digits and '-', \
                 with at least one letter (a Kubernetes port name)",
            ));
        }
        if reserved.names.contains(&p.name.as_str()) {
            return Err(invalid(
                format!("{at}.name"),
                format!(
                    "{:?} is a port name tonin already uses (reserved: {})",
                    p.name,
                    reserved.names.join(", ")
                ),
            ));
        }
        if !names.insert(p.name.clone()) {
            return Err(invalid(format!("{at}.name"), "is declared twice"));
        }
        if !(1..=65535).contains(&p.port) {
            return Err(invalid(
                format!("{at}.port"),
                format!("{} is outside 1-65535", p.port),
            ));
        }
        let protocol = p.protocol.clone().unwrap_or_else(|| "TCP".to_string());
        if !matches!(protocol.as_str(), "TCP" | "UDP" | "SCTP") {
            return Err(invalid(
                format!("{at}.protocol"),
                format!("{protocol:?} is not supported; use \"TCP\", \"UDP\" or \"SCTP\""),
            ));
        }
        if let Some((_, owner)) = reserved.numbers.iter().find(|(n, _)| *n == p.port) {
            return Err(invalid(
                format!("{at}.port"),
                format!("{} is already the {owner} port", p.port),
            ));
        }
        if !numbers.insert((p.port, protocol.clone())) {
            return Err(invalid(
                format!("{at}.port"),
                format!("{}/{protocol} is declared twice", p.port),
            ));
        }
        let svc = p.service.clone().unwrap_or_default();
        if !svc.is_empty() {
            if !is_dns_label(&svc, 63) || svc == "headless" {
                return Err(invalid(
                    format!("{at}.service"),
                    format!(
                        "{svc:?} must be a DNS-1123 label other than \"headless\" \
                         (the Service is named <service>-<suffix>)"
                    ),
                ));
            }
            let full = format!("{service}-{svc}");
            if full.len() > 63 {
                return Err(invalid(
                    format!("{at}.service"),
                    format!("Service name {full:?} is longer than 63 characters"),
                ));
            }
        }
        out.push(PortSpec {
            name: p.name.clone(),
            port: p.port,
            protocol,
            service: svc,
        });
    }
    Ok(out)
}

pub(crate) fn resolve_resources(raw: &RawResources) -> Result<ResourcesSpec, Error> {
    let shorthand = raw.cpu.is_some() || raw.memory.is_some();
    let tables = raw.requests.is_some() || raw.limits.is_some();
    if shorthand && tables {
        return Err(invalid(
            "[resources]",
            "use either `cpu`/`memory` (requests = limits) or [resources.requests] / \
             [resources.limits], not both",
        ));
    }
    let list = |table: &str, l: &RawResourceList| -> Result<BTreeMap<String, String>, Error> {
        let mut m = BTreeMap::new();
        for (key, val) in [
            ("cpu", &l.cpu),
            ("memory", &l.memory),
            ("ephemeral-storage", &l.ephemeral_storage),
        ] {
            if let Some(v) = val {
                let field = format!("{table}.{}", key.replace('-', "_"));
                m.insert(key.to_string(), quantity(&field, v)?);
            }
        }
        Ok(m)
    };
    if shorthand {
        let cpu = raw
            .cpu
            .as_deref()
            .ok_or_else(|| invalid("[resources].cpu", "required with `memory`"))?;
        let memory = raw
            .memory
            .as_deref()
            .ok_or_else(|| invalid("[resources].memory", "required with `cpu`"))?;
        let m = BTreeMap::from([
            ("cpu".to_string(), quantity("[resources].cpu", cpu)?),
            (
                "memory".to_string(),
                quantity("[resources].memory", memory)?,
            ),
        ]);
        return Ok(ResourcesSpec {
            requests: m.clone(),
            limits: m,
        });
    }
    let requests = raw
        .requests
        .as_ref()
        .ok_or_else(|| {
            invalid(
                "[resources]",
                "set `cpu` and `memory`, or a [resources.requests] table",
            )
        })
        .and_then(|r| list("[resources.requests]", r))?;
    for key in ["cpu", "memory"] {
        if !requests.contains_key(key) {
            return Err(invalid(
                format!("[resources.requests].{key}"),
                "required (the scheduler and the autoscaler need it)",
            ));
        }
    }
    let limits = match &raw.limits {
        Some(l) => list("[resources.limits]", l)?,
        None => BTreeMap::new(),
    };
    Ok(ResourcesSpec { requests, limits })
}

pub(crate) fn resolve_autoscale(
    raw: Option<&RawAutoscale>,
    kind: WorkloadKind,
    replicas: u32,
    env: &str,
) -> Result<Option<AutoscaleSpec>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    for (name, overlay) in &raw.envs {
        if overlay.min_replicas.is_none()
            && overlay.max_replicas.is_none()
            && overlay.enabled.is_none()
        {
            return Err(invalid(
                format!("[autoscale.{name}]"),
                "is empty or unknown; [autoscale] takes enabled, min_replicas, max_replicas \
                 and [autoscale.<env>] overlays of those",
            ));
        }
    }
    let o = raw.envs.get(env).cloned().unwrap_or_default();
    let enabled = o.enabled.or(raw.base.enabled).unwrap_or(true);
    if !enabled {
        return Ok(None);
    }
    if kind != WorkloadKind::Deployment {
        return Err(invalid(
            "[autoscale]",
            "only applies to kind = \"deployment\"; a StatefulSet has a fixed replica count \
             (remove [autoscale], or set enabled = false for this env)",
        ));
    }
    let max = o
        .max_replicas
        .or(raw.base.max_replicas)
        .ok_or_else(|| invalid("[autoscale].max_replicas", "required"))?;
    let min = o.min_replicas.or(raw.base.min_replicas).unwrap_or(replicas);
    if min == 0 {
        return Err(invalid("[autoscale].min_replicas", "must be at least 1"));
    }
    if min > max {
        return Err(invalid(
            "[autoscale]",
            format!("min_replicas ({min}) is greater than max_replicas ({max}) for env {env:?}"),
        ));
    }
    // A range of one replica count is no autoscaling at all.
    if min == max {
        return Ok(None);
    }
    Ok(Some(AutoscaleSpec {
        min_replicas: min,
        max_replicas: max,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities() {
        for ok in ["100m", "1", "0.5", "256Mi", "8Gi", "1.5Gi", "512M", "2k"] {
            assert!(is_quantity(ok), "{ok}");
        }
        for bad in ["", "Gi", "8 Gi", "8gb", "1.", ".5", "-1", "8GiB"] {
            assert!(!is_quantity(bad), "{bad}");
        }
    }

    #[test]
    fn port_names() {
        assert!(is_port_name("otlp-http"));
        assert!(is_port_name("query"));
        assert!(!is_port_name("8081"), "needs a letter");
        assert!(!is_port_name("a-very-long-port-name"), "over 15");
        assert!(!is_port_name("Admin"));
        assert!(!is_port_name("a--b"));
    }

    fn vol(name: &str, kind: &str, path: &str) -> RawVolume {
        RawVolume {
            name: name.into(),
            kind: kind.into(),
            mount_path: path.into(),
            read_only: None,
            size_limit: None,
            medium: None,
            size: Some("1Gi".into()).filter(|_| kind == "pvc"),
            storage_class: None,
            access_modes: None,
        }
    }

    #[test]
    fn volumes_sort_parent_first_and_reject_pvc_on_deployments() {
        let raw = vec![
            vol("cache", "empty-dir", "/data/cache"),
            vol("data", "pvc", "/data/"),
            vol("tmp", "empty-dir", "/tmp"),
        ];
        let v = resolve_volumes(WorkloadKind::StatefulSet, &raw).unwrap();
        let paths: Vec<_> = v.iter().map(|v| v.mount_path.as_str()).collect();
        assert_eq!(paths, ["/data", "/data/cache", "/tmp"]);

        let err = resolve_volumes(WorkloadKind::Deployment, &raw).unwrap_err();
        assert!(err.to_string().contains("statefulset"), "{err}");
    }

    #[test]
    fn volumes_reject_duplicates_reserved_names_and_bad_fields() {
        let dup = vec![vol("a", "empty-dir", "/a"), vol("a", "empty-dir", "/b")];
        assert!(resolve_volumes(WorkloadKind::Deployment, &dup).is_err());
        let same_path = vec![vol("a", "empty-dir", "/a"), vol("b", "empty-dir", "/a/")];
        assert!(resolve_volumes(WorkloadKind::Deployment, &same_path).is_err());
        let reserved = vec![vol("pg-ca-x", "empty-dir", "/a")];
        assert!(resolve_volumes(WorkloadKind::Deployment, &reserved).is_err());
        let relative = vec![vol("a", "empty-dir", "tmp")];
        assert!(resolve_volumes(WorkloadKind::Deployment, &relative).is_err());
        let mut no_size = vol("data", "pvc", "/data");
        no_size.size = None;
        assert!(resolve_volumes(WorkloadKind::StatefulSet, &[no_size]).is_err());
        let mut size_on_empty = vol("tmp", "empty-dir", "/tmp");
        size_on_empty.size = Some("1Gi".into());
        assert!(resolve_volumes(WorkloadKind::Deployment, &[size_on_empty]).is_err());
        let unknown = vec![vol("a", "hostPath", "/a")];
        assert!(resolve_volumes(WorkloadKind::Deployment, &unknown).is_err());
    }

    fn port(name: &str, port: u32, service: Option<&str>) -> RawPort {
        RawPort {
            name: name.into(),
            port,
            protocol: None,
            service: service.map(Into::into),
        }
    }

    #[test]
    fn ports_validate_names_numbers_and_services() {
        let reserved = ReservedPorts {
            names: vec!["grpc", "http"],
            numbers: vec![(4317, "service"), (8080, "[service.http]")],
        };
        let ok = resolve_ports(
            "zradar",
            &[
                port("otlp-http", 4318, Some("ingest")),
                port("query", 8081, None),
            ],
            &reserved,
        )
        .unwrap();
        assert_eq!(ok[0].protocol, "TCP");
        assert_eq!(ok[0].service, "ingest");
        assert_eq!(ok[1].service, "");

        for bad in [
            vec![port("grpc", 9000, None)],
            vec![port("x", 4317, None)],
            vec![port("x", 9000, None), port("x", 9001, None)],
            vec![port("x", 9000, None), port("y", 9000, None)],
            vec![port("x", 9000, Some("headless"))],
            vec![port("x", 9000, Some("Bad_Name"))],
            vec![port("x", 0, None)],
        ] {
            assert!(resolve_ports("zradar", &bad, &reserved).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn resources_shorthand_and_tables() {
        let short = RawResources {
            cpu: Some("100m".into()),
            memory: Some("128Mi".into()),
            requests: None,
            limits: None,
        };
        let r = resolve_resources(&short).unwrap();
        assert_eq!(r.requests, r.limits);

        let tables = RawResources {
            cpu: None,
            memory: None,
            requests: Some(RawResourceList {
                cpu: Some("250m".into()),
                memory: Some("512Mi".into()),
                ephemeral_storage: Some("1Gi".into()),
            }),
            limits: Some(RawResourceList {
                cpu: None,
                memory: Some("512Mi".into()),
                ephemeral_storage: None,
            }),
        };
        let r = resolve_resources(&tables).unwrap();
        assert_eq!(r.requests["ephemeral-storage"], "1Gi");
        assert!(!r.limits.contains_key("cpu"), "CPU limit is optional");

        let both = RawResources {
            cpu: Some("1".into()),
            ..tables.clone()
        };
        assert!(resolve_resources(&both).is_err());
        let no_requests = RawResources {
            requests: None,
            ..tables
        };
        assert!(resolve_resources(&no_requests).is_err());
    }

    fn autoscale(min: Option<u32>, max: u32) -> RawAutoscale {
        RawAutoscale {
            base: RawAutoscaleFields {
                enabled: None,
                min_replicas: min,
                max_replicas: Some(max),
            },
            envs: BTreeMap::new(),
        }
    }

    #[test]
    fn autoscale_min_replicas_and_env_overlay() {
        let mut a = autoscale(Some(2), 8);
        let s = resolve_autoscale(Some(&a), WorkloadKind::Deployment, 1, "prd")
            .unwrap()
            .unwrap();
        assert_eq!((s.min_replicas, s.max_replicas), (2, 8));
        // Without min_replicas the floor is the replica count.
        let s = resolve_autoscale(
            Some(&autoscale(None, 4)),
            WorkloadKind::Deployment,
            2,
            "prd",
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.min_replicas, 2);

        a.envs.insert(
            "dev".into(),
            RawAutoscaleFields {
                enabled: Some(false),
                ..Default::default()
            },
        );
        assert!(
            resolve_autoscale(Some(&a), WorkloadKind::Deployment, 1, "dev")
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_autoscale(
                Some(&autoscale(Some(5), 4)),
                WorkloadKind::Deployment,
                1,
                "prd"
            )
            .is_err()
        );
        let err = resolve_autoscale(
            Some(&autoscale(None, 4)),
            WorkloadKind::StatefulSet,
            2,
            "prd",
        )
        .unwrap_err();
        assert!(err.to_string().contains("deployment"), "{err}");
    }

    #[test]
    fn strategy_per_kind() {
        let sts = RawStrategy {
            partition: Some(1),
            ..Default::default()
        };
        let s = resolve_strategy(WorkloadKind::StatefulSet, Some(&sts), None, "prd")
            .unwrap()
            .unwrap();
        assert_eq!(s.kind, "RollingUpdate");
        assert_eq!(
            s.rolling_update["partition"],
            serde_yaml_value::Value::Int(1)
        );
        assert!(resolve_strategy(WorkloadKind::Deployment, Some(&sts), None, "prd").is_err());

        let dep = RawStrategy {
            max_surge: Some(toml::Value::String("25%".into())),
            max_unavailable: Some(toml::Value::Integer(0)),
            ..Default::default()
        };
        let s = resolve_strategy(WorkloadKind::Deployment, Some(&dep), None, "prd")
            .unwrap()
            .unwrap();
        assert_eq!(s.rolling_update.len(), 2);
        assert!(resolve_strategy(WorkloadKind::StatefulSet, Some(&dep), None, "prd").is_err());
        // The env overlay wins field by field.
        let prd = RawStrategy {
            kind: Some("Recreate".into()),
            ..Default::default()
        };
        assert!(
            resolve_strategy(WorkloadKind::Deployment, Some(&dep), Some(&prd), "prd").is_err(),
            "maxSurge with Recreate"
        );
        assert!(
            resolve_strategy(WorkloadKind::Deployment, None, None, "prd")
                .unwrap()
                .is_none()
        );
    }
}
