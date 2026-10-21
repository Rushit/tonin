//! Values for pod hygiene: pull secrets, placement, PDB, ServiceAccount and
//! CronJobs (`tonin_plugin::PodSettings`).
//!
//! The chart reads them through the `service.podSpecCommon` helper (every pod
//! spec), `serviceaccount.yaml`, `pdb.yaml` and `cronjob.yaml`.

use std::collections::BTreeMap;

use serde::Serialize;
use tonin_plugin::{CronJobSpec, PdbSpec, Plan, TokenVolume};

#[derive(Serialize)]
struct NameRef<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct PdbValues<'a> {
    enabled: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    budget: Option<&'a PdbSpec>,
}

/// Per-env keys, rendered into `values-<env>.yaml`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PodValues<'a> {
    image_pull_secrets: Vec<NameRef<'a>>,
    node_selector: &'a BTreeMap<String, String>,
    tolerations: &'a [toml::Value],
    topology_spread_constraints: &'a [toml::Value],
    pod_disruption_budget: PdbValues<'a>,
    cronjobs: &'a [CronJobSpec],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceAccountValues {
    automount_token: bool,
    token_volumes: Vec<TokenVolume>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceAccountBlock {
    service_account: ServiceAccountValues,
}

fn to_yaml<T: Serialize>(value: &T) -> String {
    let raw = serde_yaml::to_string(value).unwrap_or_default();
    raw.strip_prefix("---\n")
        .unwrap_or(&raw)
        .trim_end()
        .to_string()
}

/// Inserts `pod_block` (per-env values), `service_account_block` (base
/// values; env-independent) and `secrets_create`.
pub(super) fn contribute(plan: &Plan, ctx: &mut tera::Context) {
    let pod = &plan.pod;
    let values = PodValues {
        image_pull_secrets: pod
            .image_pull_secrets
            .iter()
            .map(|name| NameRef { name })
            .collect(),
        node_selector: &pod.placement.node_selector,
        tolerations: &pod.placement.tolerations,
        topology_spread_constraints: &pod.placement.topology_spread,
        pod_disruption_budget: PdbValues {
            enabled: pod.pdb.is_some(),
            budget: pod.pdb.as_ref(),
        },
        cronjobs: &pod.cronjobs,
    };
    ctx.insert("pod_block", &to_yaml(&values));

    let sa = ServiceAccountBlock {
        service_account: ServiceAccountValues {
            automount_token: pod.service_account.automount_token,
            token_volumes: pod.service_account.token_volumes(),
        },
    };
    ctx.insert("service_account_block", &to_yaml(&sa));

    let secrets_create = plan.secrets.as_ref().is_none_or(|s| s.create);
    ctx.insert("secrets_create", &secrets_create);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn chart_templates() -> Vec<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/helm/chart-templates");
        let mut out: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    std::fs::read_to_string(&path).unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// Every pod spec tonin renders (Deployment, StatefulSet, Job, CronJob)
    /// gets ServiceAccount, pull secrets, security context and placement from
    /// the one helper, never field by field.
    #[test]
    fn every_pod_spec_includes_the_common_helper() {
        for (name, src) in chart_templates() {
            if name == "_helpers.tpl" {
                continue;
            }
            let pod_specs = src.matches("containers:").count();
            let includes = src.matches(r#"include "service.podSpecCommon""#).count();
            assert_eq!(
                pod_specs, includes,
                "{name}: {pod_specs} pod spec(s) but {includes} `service.podSpecCommon` include(s)"
            );
            for field in [
                "imagePullSecrets",
                "nodeSelector",
                "tolerations",
                "serviceAccountName",
                ".Values.podSecurityContext",
            ] {
                assert!(
                    !src.contains(field),
                    "{name} sets {field} itself; use `service.podSpecCommon`"
                );
            }
        }
    }

    fn plan(toml: &str, env: &str) -> Plan {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tonin.toml");
        std::fs::write(&path, toml).unwrap();
        Plan::load_with_env(&path, env).unwrap()
    }

    const TOML: &str = r#"
[service]
name    = "identity"
version = "0.1.0"

[deploy]
replicas  = 2
namespace = "platform"

[deploy.prd]
image_pull_secrets = ["ghcr-read"]

[resources]
cpu    = "100m"
memory = "128Mi"

[placement.prd]
node_selector = { "example.com/role" = "app" }

[pdb]
max_unavailable = "50%"

[service_account]
token = { audience = "identity" }

[secrets]
required = ["JWT_SECRET"]

[secrets.prd]
create = false

[[cronjobs]]
name     = "purge"
schedule = "*/5 * * * *"
args     = ["purge"]
"#;

    fn ctx_str(ctx: &tera::Context, key: &str) -> String {
        ctx.get(key).unwrap().as_str().unwrap().to_string()
    }

    #[test]
    fn values_carry_the_resolved_pod_settings() {
        let mut ctx = tera::Context::new();
        contribute(&plan(TOML, "prd"), &mut ctx);
        let pod = ctx_str(&ctx, "pod_block");
        for expected in [
            "imagePullSecrets:\n- name: ghcr-read",
            "nodeSelector:\n  example.com/role: app",
            "topologySpreadConstraints:\n- maxSkew: 1\n  topologyKey: kubernetes.io/hostname\n  whenUnsatisfiable: ScheduleAnyway",
            "podDisruptionBudget:\n  enabled: true\n  maxUnavailable: 50%",
            "- name: purge\n  schedule: '*/5 * * * *'",
        ] {
            assert!(pod.contains(expected), "missing {expected:?}:\n{pod}");
        }
        let sa = ctx_str(&ctx, "service_account_block");
        assert!(
            sa.contains(
                "mountPath: /var/run/secrets/tokens\n    sources:\n    - audience: identity"
            ),
            "{sa}"
        );
        assert_eq!(ctx.get("secrets_create").unwrap(), false);

        let mut dev = tera::Context::new();
        contribute(&plan(TOML, "dev"), &mut dev);
        let pod = ctx_str(&dev, "pod_block");
        assert!(
            pod.contains("imagePullSecrets: []\nnodeSelector: {}"),
            "{pod}"
        );
        assert_eq!(dev.get("secrets_create").unwrap(), true);
    }

    #[test]
    fn no_pdb_section_renders_it_disabled() {
        let toml = TOML.replace("[pdb]\nmax_unavailable = \"50%\"\n", "");
        let mut ctx = tera::Context::new();
        contribute(&plan(&toml, "prd"), &mut ctx);
        assert!(
            ctx_str(&ctx, "pod_block")
                .contains("podDisruptionBudget:\n  enabled: false\ncronjobs:"),
        );
    }
}
