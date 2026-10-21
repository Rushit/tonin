//! Network policy inputs: `[callers]` (ingress) and `[egress]` (non-service
//! egress: FQDNs, CIDRs, the kube-apiserver). `[depends_on]` (egress to other
//! tonin services) lives in `plan.rs` because it also drives deploy waves.
//!
//! Both sections are parsed from raw TOML values so every error names the
//! offending entry and the keys that are allowed there.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::plan::{Error, ServiceRef, apply_env, ensure_resolved};

// ---------- shared helpers ----------

fn invalid(field: &str, reason: impl Into<String>) -> Error {
    Error::InvalidField {
        field: field.to_string(),
        reason: reason.into(),
    }
}

/// Parse a non-empty list of TCP ports (`ports = [443, 8443]`).
pub(crate) fn parse_ports(field: &str, value: &toml::Value) -> Result<Vec<u32>, Error> {
    let arr = value
        .as_array()
        .ok_or_else(|| invalid(field, "`ports` must be an array of port numbers"))?;
    if arr.is_empty() {
        return Err(invalid(field, "`ports` must not be empty"));
    }
    let mut ports = Vec::with_capacity(arr.len());
    for item in arr {
        let p = item
            .as_integer()
            .ok_or_else(|| invalid(field, "`ports` must be an array of port numbers"))?;
        if !(1..=65535).contains(&p) {
            return Err(invalid(field, format!("port {p} is outside 1-65535")));
        }
        let p = p as u32;
        if ports.contains(&p) {
            return Err(invalid(field, format!("port {p} is listed twice")));
        }
        ports.push(p);
    }
    Ok(ports)
}

fn as_str<'a>(field: &str, key: &str, value: &'a toml::Value) -> Result<&'a str, Error> {
    value
        .as_str()
        .ok_or_else(|| invalid(field, format!("`{key}` must be a string")))
}

fn unknown_key(field: &str, key: &str, allowed: &[&str]) -> Error {
    invalid(
        field,
        format!(
            "unknown key `{key}` (allowed: {})",
            allowed
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

// ---------- [callers] ----------

/// Keys of a caller table. A `[callers]` sub-table with none of them is an
/// environment overlay (`[callers.<env>]`), never a caller.
pub const CALLER_KEYS: &[&str] = &["namespace", "labels", "service", "ports"];

/// Where a caller's traffic comes from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum CallerSource {
    /// Another tonin service, matched by its `service.identity` label.
    Service(ServiceRef),
    /// Any pods with these labels, in `namespace` (or the service's own
    /// namespace when `None`): e.g. cloudflared, an ingress controller.
    Pods {
        namespace: Option<String>,
        labels: BTreeMap<String, String>,
    },
}

/// One resolved `[callers]` entry: who may call this service, and on which
/// ports (empty = the default set: the main port, the HTTP port and the MCP
/// port when enabled).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Caller {
    /// The `[callers]` key.
    pub name: String,
    pub source: CallerSource,
    pub ports: Vec<u32>,
}

impl Caller {
    /// A service caller on the default ports (also what workspace loading
    /// derives from other services' `[depends_on]`).
    pub fn from_service(service: ServiceRef) -> Self {
        Self {
            name: service.name.clone(),
            source: CallerSource::Service(service),
            ports: Vec::new(),
        }
    }

    /// The calling tonin service, or `None` for a namespace/label caller.
    /// Only service callers take part in `tonin affected` propagation.
    pub fn service(&self) -> Option<&ServiceRef> {
        match &self.source {
            CallerSource::Service(s) => Some(s),
            CallerSource::Pods { .. } => None,
        }
    }
}

/// Is this `[callers]` value an env overlay (`[callers.<env>]`)? A table
/// with none of the caller keys is one; anything else is a caller.
fn is_overlay(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(t) => !t.keys().any(|k| CALLER_KEYS.contains(&k.as_str())),
        _ => false,
    }
}

/// Parse one caller entry (shorthand namespace string or caller table) for `env`.
fn parse_caller(field: &str, name: &str, value: &toml::Value, env: &str) -> Result<Caller, Error> {
    let resolve_ns = |raw: &str| -> Result<String, Error> {
        let ns = apply_env(raw, env);
        ensure_resolved(field, &ns)?;
        if ns.is_empty() {
            return Err(invalid(field, "`namespace` must not be empty"));
        }
        Ok(ns)
    };
    match value {
        toml::Value::String(ns) => Ok(Caller::from_service(ServiceRef {
            name: name.to_string(),
            namespace: resolve_ns(ns)?,
        })),
        toml::Value::Table(table) => {
            let mut namespace = None;
            let mut labels: Option<BTreeMap<String, String>> = None;
            let mut service = None;
            let mut ports = Vec::new();
            for (key, val) in table {
                match key.as_str() {
                    "namespace" => namespace = Some(resolve_ns(as_str(field, key, val)?)?),
                    "service" => service = Some(as_str(field, key, val)?.to_string()),
                    "ports" => ports = parse_ports(field, val)?,
                    "labels" => {
                        let t = val.as_table().ok_or_else(|| {
                            invalid(field, "`labels` must be a table of string values")
                        })?;
                        if t.is_empty() {
                            return Err(invalid(field, "`labels` must not be empty"));
                        }
                        let mut map = BTreeMap::new();
                        for (k, v) in t {
                            let v = v.as_str().ok_or_else(|| {
                                invalid(field, format!("label `{k}` must be a string"))
                            })?;
                            map.insert(k.clone(), v.to_string());
                        }
                        labels = Some(map);
                    }
                    other => return Err(unknown_key(field, other, CALLER_KEYS)),
                }
            }
            let source = match (labels, service) {
                (Some(_), Some(_)) => {
                    return Err(invalid(
                        field,
                        "`labels` and `service` are exclusive: a caller is either a tonin \
                         service or a set of pod labels",
                    ));
                }
                (Some(labels), None) => CallerSource::Pods { namespace, labels },
                (None, service) => {
                    let Some(namespace) = namespace else {
                        return Err(invalid(
                            field,
                            "a service caller needs `namespace` (or give `labels` to match pods)",
                        ));
                    };
                    CallerSource::Service(ServiceRef {
                        name: service.unwrap_or_else(|| name.to_string()),
                        namespace,
                    })
                }
            };
            Ok(Caller {
                name: name.to_string(),
                source,
                ports,
            })
        }
        other => Err(invalid(
            field,
            format!(
                "expected a namespace string or a caller table, got {}",
                other.type_str()
            ),
        )),
    }
}

/// Resolve `[callers]` for one environment.
///
/// - `name = "<ns>"`: the tonin service `name` in `<ns>` (default ports).
/// - `name = { namespace, labels, service, ports }`: a caller table. With
///   `labels`, any pods carrying them (in `namespace`, or the service's own
///   namespace); otherwise the tonin service `service` (default: `name`) in
///   `namespace`. `ports` replaces the default port set.
/// - `[callers.<env>]`: a table with none of the caller keys is an overlay;
///   its entries (same two forms) add to or replace the base entries of the
///   same name in that environment.
///
/// `{env}` in namespaces is substituted. Overlays for other environments are
/// still validated, so a mistake fails in every environment.
pub(crate) fn resolve_callers(
    raw: &BTreeMap<String, toml::Value>,
    env: &str,
) -> Result<Vec<Caller>, Error> {
    let mut base = BTreeMap::new();
    let mut overlay = BTreeMap::new();
    for (key, value) in raw {
        if is_overlay(value) {
            let Some(table) = value.as_table() else {
                continue;
            };
            for (name, entry) in table {
                let field = format!("callers.{key}.{name}");
                if is_overlay(entry) {
                    return Err(invalid(
                        &field,
                        format!(
                            "a caller table needs at least one of {}; overlays do not nest",
                            CALLER_KEYS.join(", ")
                        ),
                    ));
                }
                let caller = parse_caller(&field, name, entry, env)?;
                if key == env {
                    overlay.insert(name.clone(), caller);
                }
            }
        } else {
            let caller = parse_caller(&format!("callers.{key}"), key, value, env)?;
            base.insert(key.clone(), caller);
        }
    }
    base.extend(overlay);
    Ok(base.into_values().collect())
}

// ---------- [egress] ----------

/// Keys of `[egress]` (and of each `[egress.<env>]` overlay).
pub const EGRESS_KEYS: &[&str] = &["fqdns", "cidrs", "kube_apiserver"];

/// Egress to a DNS name (Cilium `toFQDNs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FqdnEgress {
    /// Exact name (`matchName`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Wildcard pattern (`matchPattern`, `*` matches within DNS labels).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub ports: Vec<u32>,
}

/// Egress to an IP range (Cilium `toCIDRSet`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CidrEgress {
    pub cidr: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub except: Vec<String>,
    pub ports: Vec<u32>,
}

/// Resolved `[egress]` for one environment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressSpec {
    pub fqdns: Vec<FqdnEgress>,
    pub cidrs: Vec<CidrEgress>,
    /// Allow the Kubernetes API server (Cilium `toEntities: [kube-apiserver]`).
    pub kube_apiserver: bool,
}

impl EgressSpec {
    pub fn is_empty(&self) -> bool {
        self.fqdns.is_empty() && self.cidrs.is_empty() && !self.kube_apiserver
    }
}

fn valid_dns_chars(s: &str, allow_wildcard: bool) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '-' || c == '.' || (allow_wildcard && c == '*')
        })
}

fn parse_fqdn(field: &str, value: &toml::Value) -> Result<FqdnEgress, Error> {
    const KEYS: &[&str] = &["name", "pattern", "ports"];
    let table = value
        .as_table()
        .ok_or_else(|| invalid(field, "expected a table `{ name = \"…\", ports = [443] }`"))?;
    let mut out = FqdnEgress::default();
    let mut ports = None;
    for (key, val) in table {
        match key.as_str() {
            "name" => {
                let s = as_str(field, key, val)?;
                if !valid_dns_chars(s, false) {
                    return Err(invalid(
                        field,
                        format!("`name` {s:?} is not a DNS name (use `pattern` for wildcards)"),
                    ));
                }
                out.name = Some(s.to_string());
            }
            "pattern" => {
                let s = as_str(field, key, val)?;
                if !valid_dns_chars(s, true) {
                    return Err(invalid(
                        field,
                        format!("`pattern` {s:?} may only hold letters, digits, `-`, `.` and `*`"),
                    ));
                }
                out.pattern = Some(s.to_string());
            }
            "ports" => ports = Some(parse_ports(field, val)?),
            other => return Err(unknown_key(field, other, KEYS)),
        }
    }
    if out.name.is_some() == out.pattern.is_some() {
        return Err(invalid(field, "give exactly one of `name` and `pattern`"));
    }
    out.ports = ports.ok_or_else(|| invalid(field, "`ports` is required"))?;
    Ok(out)
}

/// Validate an `ip/prefix` CIDR (IPv4 or IPv6).
fn check_cidr(field: &str, cidr: &str) -> Result<(), Error> {
    let bad = || {
        invalid(
            field,
            format!("{cidr:?} is not a CIDR (`10.0.0.0/8`, `fd00::/8`)"),
        )
    };
    let (ip, prefix) = cidr.split_once('/').ok_or_else(bad)?;
    let ip: std::net::IpAddr = ip.parse().map_err(|_| bad())?;
    let prefix: u8 = prefix.parse().map_err(|_| bad())?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return Err(bad());
    }
    Ok(())
}

fn parse_cidr(field: &str, value: &toml::Value) -> Result<CidrEgress, Error> {
    const KEYS: &[&str] = &["cidr", "except", "ports"];
    let table = value
        .as_table()
        .ok_or_else(|| invalid(field, "expected a table `{ cidr = \"…\", ports = [443] }`"))?;
    let mut cidr = None;
    let mut except = Vec::new();
    let mut ports = None;
    for (key, val) in table {
        match key.as_str() {
            "cidr" => {
                let s = as_str(field, key, val)?;
                check_cidr(field, s)?;
                cidr = Some(s.to_string());
            }
            "except" => {
                let arr = val
                    .as_array()
                    .ok_or_else(|| invalid(field, "`except` must be an array of CIDRs"))?;
                for item in arr {
                    let s = item
                        .as_str()
                        .ok_or_else(|| invalid(field, "`except` must be an array of CIDRs"))?;
                    check_cidr(field, s)?;
                    except.push(s.to_string());
                }
            }
            "ports" => ports = Some(parse_ports(field, val)?),
            other => return Err(unknown_key(field, other, KEYS)),
        }
    }
    Ok(CidrEgress {
        cidr: cidr.ok_or_else(|| invalid(field, "`cidr` is required"))?,
        except,
        ports: ports.ok_or_else(|| invalid(field, "`ports` is required"))?,
    })
}

fn parse_list<T>(
    field: &str,
    value: &toml::Value,
    item: impl Fn(&str, &toml::Value) -> Result<T, Error>,
) -> Result<Vec<T>, Error> {
    let arr = value
        .as_array()
        .ok_or_else(|| invalid(field, "must be an array of tables"))?;
    arr.iter()
        .enumerate()
        .map(|(i, v)| item(&format!("{field}[{i}]"), v))
        .collect()
}

/// Apply the keys of one `[egress]` table (base or overlay) onto `spec`;
/// each key present replaces the value it had.
fn apply_egress_table(
    prefix: &str,
    table: &toml::map::Map<String, toml::Value>,
    spec: &mut EgressSpec,
    allow_overlays: bool,
) -> Result<(), Error> {
    for (key, val) in table {
        let field = format!("{prefix}.{key}");
        match key.as_str() {
            "fqdns" => spec.fqdns = parse_list(&field, val, parse_fqdn)?,
            "cidrs" => spec.cidrs = parse_list(&field, val, parse_cidr)?,
            "kube_apiserver" => {
                spec.kube_apiserver = val
                    .as_bool()
                    .ok_or_else(|| invalid(&field, "must be true or false"))?;
            }
            _ if allow_overlays && val.is_table() => {} // [egress.<env>], applied after
            other => return Err(unknown_key(prefix, other, EGRESS_KEYS)),
        }
    }
    Ok(())
}

/// Resolve `[egress]` for one environment: the base keys, then the
/// `[egress.<env>]` overlay (each key it sets replaces the base value).
/// Overlays for other environments are still validated.
pub(crate) fn resolve_egress(
    raw: &toml::map::Map<String, toml::Value>,
    env: &str,
) -> Result<EgressSpec, Error> {
    let mut spec = EgressSpec::default();
    apply_egress_table("egress", raw, &mut spec, true)?;
    for (key, val) in raw {
        let Some(overlay) = val.as_table() else {
            continue;
        };
        let mut scratch = EgressSpec::default();
        let target = if key == env { &mut spec } else { &mut scratch };
        apply_egress_table(&format!("egress.{key}"), overlay, target, false)?;
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callers(src: &str, env: &str) -> Result<Vec<Caller>, Error> {
        let v: toml::Table = toml::from_str(src).unwrap();
        let raw: BTreeMap<String, toml::Value> = match v.get("callers") {
            Some(toml::Value::Table(t)) => t.clone().into_iter().collect(),
            _ => BTreeMap::new(),
        };
        resolve_callers(&raw, env)
    }

    fn ok(src: &str, env: &str) -> Vec<Caller> {
        callers(src, env).unwrap()
    }

    fn err(src: &str, env: &str) -> String {
        callers(src, env).unwrap_err().to_string()
    }

    fn ns(c: &Caller) -> &str {
        match &c.source {
            CallerSource::Service(s) => &s.namespace,
            CallerSource::Pods { namespace, .. } => namespace.as_deref().unwrap_or(""),
        }
    }

    #[test]
    fn callers_base_only_no_overlay() {
        let c = ok(
            "[callers]\norders-service = \"myapp\"\ninventory-service = \"myapp\"\n",
            "dev",
        );
        assert_eq!(c.len(), 2);
        assert!(c.iter().all(|c| ns(c) == "myapp" && c.ports.is_empty()));
        assert!(c.iter().all(|c| c.service().is_some()));
    }

    #[test]
    fn callers_env_overlay_overrides_and_adds() {
        let src = "[callers]\norders-service = \"myapp\"\n\
                   [callers.dev]\norders-service = \"myapp-dev\"\ndebug-tool = \"myapp-dev\"\n";
        let dev = ok(src, "dev");
        assert_eq!(dev.len(), 2, "overlay adds debug-tool");
        assert!(dev.iter().all(|c| ns(c) == "myapp-dev"), "dev overlay wins");
        let prd = ok(src, "prd");
        assert_eq!(prd.len(), 1);
        assert_eq!(ns(&prd[0]), "myapp", "prd falls back to the base");
    }

    #[test]
    fn callers_env_placeholder_with_prod_override() {
        let src = "[callers]\norders = \"myapp-{env}\"\n[callers.prod]\norders = \"myapp\"\n";
        assert_eq!(ns(&ok(src, "dev")[0]), "myapp-dev");
        assert_eq!(ns(&ok(src, "staging")[0]), "myapp-staging");
        assert_eq!(ns(&ok(src, "prod")[0]), "myapp");
    }

    #[test]
    fn caller_table_with_labels_is_a_pod_caller() {
        let c = ok(
            r#"[callers]
cloudflared = { namespace = "edge", labels = { "app.kubernetes.io/name" = "cloudflared" }, ports = [4318] }
"#,
            "dev",
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "cloudflared");
        assert_eq!(c[0].ports, vec![4318]);
        assert!(c[0].service().is_none(), "label callers are not services");
        let CallerSource::Pods { namespace, labels } = &c[0].source else {
            panic!("expected a pod caller");
        };
        assert_eq!(namespace.as_deref(), Some("edge"));
        assert_eq!(labels["app.kubernetes.io/name"], "cloudflared");
    }

    #[test]
    fn caller_table_as_a_section_is_not_an_overlay() {
        // `[callers.cloudflared]` with caller keys is a caller, not an env overlay.
        let c = ok(
            "[callers.cloudflared]\nnamespace = \"edge\"\nports = [4318]\n\
             [callers.cloudflared.labels]\n\"app.kubernetes.io/name\" = \"cloudflared\"\n",
            "prd",
        );
        assert_eq!(c.len(), 1);
        assert!(c[0].service().is_none());
    }

    #[test]
    fn label_caller_without_namespace_matches_own_namespace() {
        let c = ok(
            "[callers]\nworker = { labels = { app = \"worker\" } }\n",
            "dev",
        );
        let CallerSource::Pods { namespace, .. } = &c[0].source else {
            panic!("expected a pod caller");
        };
        assert!(namespace.is_none());
    }

    #[test]
    fn service_caller_table_with_ports_and_alias() {
        let c = ok(
            "[callers]\nshell = { service = \"ui-shell\", namespace = \"ui-{env}\", ports = [7001] }\n",
            "prd",
        );
        let svc = c[0].service().unwrap();
        assert_eq!(svc.name, "ui-shell");
        assert_eq!(svc.namespace, "ui-prd");
        assert_eq!(c[0].ports, vec![7001]);
    }

    #[test]
    fn overlay_may_hold_table_callers() {
        let src = "[callers]\nui = \"ui-{env}\"\n\
                   [callers.prd]\ncloudflared = { namespace = \"edge\", labels = { app = \"cf\" } }\n";
        assert_eq!(ok(src, "dev").len(), 1);
        assert_eq!(ok(src, "prd").len(), 2);
    }

    #[test]
    fn caller_errors_name_the_entry() {
        let e = err(
            "[callers]\ncf = { namespace = \"edge\", lables = { app = \"cf\" } }\n",
            "dev",
        );
        assert!(
            e.contains("callers.cf") && e.contains("unknown key `lables`"),
            "{e}"
        );
        assert!(e.contains("`labels`"), "lists allowed keys: {e}");

        let e = err("[callers]\ncf = { ports = [443] }\n", "dev");
        assert!(e.contains("needs `namespace`"), "{e}");

        let e = err(
            "[callers]\ncf = { namespace = \"e\", service = \"x\", labels = { a = \"b\" } }\n",
            "dev",
        );
        assert!(e.contains("exclusive"), "{e}");

        let e = err("[callers]\ncf = { namespace = \"e\", ports = [] }\n", "dev");
        assert!(e.contains("must not be empty"), "{e}");

        let e = err(
            "[callers]\ncf = { namespace = \"e\", ports = [70000] }\n",
            "dev",
        );
        assert!(e.contains("outside 1-65535"), "{e}");

        let e = err(
            "[callers]\ncf = { namespace = \"e\", labels = {} }\n",
            "dev",
        );
        assert!(e.contains("`labels` must not be empty"), "{e}");

        let e = err("[callers]\ncf = 5\n", "dev");
        assert!(e.contains("expected a namespace string"), "{e}");

        let e = err("[callers]\ncf = \"ns-{envv}\"\n", "dev");
        assert!(e.contains("unresolved placeholder"), "{e}");
    }

    #[test]
    fn inactive_overlay_is_still_validated() {
        let e = err(
            "[callers]\n[callers.prd]\ncf = { namespace = \"e\", ports = [0] }\n",
            "dev",
        );
        assert!(e.contains("callers.prd.cf"), "{e}");
    }

    #[test]
    fn nested_overlays_are_rejected() {
        let e = err("[callers.prd.staging]\nx = \"ns\"\n", "dev");
        assert!(e.contains("overlays do not nest"), "{e}");
    }

    // ---- [egress] ----

    fn egress(src: &str, env: &str) -> Result<EgressSpec, Error> {
        let v: toml::Table = toml::from_str(src).unwrap();
        let raw = match v.get("egress") {
            Some(toml::Value::Table(t)) => t.clone(),
            _ => toml::map::Map::new(),
        };
        resolve_egress(&raw, env)
    }

    #[test]
    fn egress_parses_fqdns_cidrs_and_apiserver() {
        let e = egress(
            r#"[egress]
fqdns = [{ name = "api.stripe.com", ports = [443] }, { pattern = "*.ovh.us", ports = [443] }]
cidrs = [{ cidr = "10.0.0.0/8", except = ["10.96.0.0/12"], ports = [5432] }]
kube_apiserver = true
"#,
            "dev",
        )
        .unwrap();
        assert_eq!(e.fqdns.len(), 2);
        assert_eq!(e.fqdns[0].name.as_deref(), Some("api.stripe.com"));
        assert_eq!(e.fqdns[1].pattern.as_deref(), Some("*.ovh.us"));
        assert_eq!(e.cidrs[0].except, vec!["10.96.0.0/12"]);
        assert!(e.kube_apiserver);
    }

    #[test]
    fn egress_env_overlay_replaces_keys() {
        let src = r#"[egress]
fqdns = [{ name = "api.stripe.com", ports = [443] }]
[egress.prd]
fqdns = [{ name = "s3.example.com", ports = [443] }]
kube_apiserver = true
"#;
        let dev = egress(src, "dev").unwrap();
        assert_eq!(dev.fqdns[0].name.as_deref(), Some("api.stripe.com"));
        assert!(!dev.kube_apiserver);
        let prd = egress(src, "prd").unwrap();
        assert_eq!(prd.fqdns.len(), 1);
        assert_eq!(prd.fqdns[0].name.as_deref(), Some("s3.example.com"));
        assert!(prd.kube_apiserver);
    }

    #[test]
    fn egress_errors() {
        let cases = [
            (
                "[egress]\nfqdns = [{ name = \"a.com\" }]\n",
                "`ports` is required",
            ),
            (
                "[egress]\nfqdns = [{ name = \"a.com\", pattern = \"*.a.com\", ports = [443] }]\n",
                "exactly one of",
            ),
            (
                "[egress]\nfqdns = [{ name = \"*.a.com\", ports = [443] }]\n",
                "use `pattern` for wildcards",
            ),
            (
                "[egress]\nfqdns = [{ pattern = \"a_b.com\", ports = [443] }]\n",
                "may only hold",
            ),
            (
                "[egress]\nfqdns = [{ host = \"a.com\", ports = [443] }]\n",
                "unknown key `host`",
            ),
            (
                "[egress]\ncidrs = [{ cidr = \"10.0.0.0/33\", ports = [443] }]\n",
                "is not a CIDR",
            ),
            (
                "[egress]\ncidrs = [{ cidr = \"10.0.0.0\", ports = [443] }]\n",
                "is not a CIDR",
            ),
            (
                "[egress]\ncidrs = [{ cidr = \"10.0.0.0/8\", except = [\"x\"], ports = [1] }]\n",
                "is not a CIDR",
            ),
            (
                "[egress]\ncidrs = [{ ports = [443] }]\n",
                "`cidr` is required",
            ),
            ("[egress]\nkube_apiserver = \"yes\"\n", "true or false"),
            ("[egress]\nfqdn = []\n", "unknown key `fqdn`"),
            (
                "[egress.prd]\nfqdns = [{ name = \"a.com\", ports = [0] }]\n",
                "egress.prd.fqdns[0]",
            ),
            ("[egress.prd.x]\nfqdns = []\n", "unknown key `x`"),
        ];
        for (src, want) in cases {
            let e = egress(src, "dev").unwrap_err().to_string();
            assert!(e.contains(want), "{src}: expected {want:?} in {e:?}");
        }
    }

    #[test]
    fn ipv6_cidr_is_accepted() {
        let e = egress(
            "[egress]\ncidrs = [{ cidr = \"fd00::/8\", ports = [443] }]\n",
            "dev",
        )
        .unwrap();
        assert_eq!(e.cidrs[0].cidr, "fd00::/8");
    }
}
