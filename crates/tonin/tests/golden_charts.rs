//! Golden tests for `tonin helm generate`.
//!
//! For every fixture under `tests/fixtures/charts/<name>/tonin.toml` this
//! renders the chart with the real `tonin` binary, then:
//!
//! 1. snapshots the generated `Chart.yaml` and values files (no tools needed);
//! 2. for each env, runs `helm template <release> <chart> -f values.yaml
//!    -f values-<env>.yaml --namespace <ns>` (the same invocation as
//!    `tonin helm template`) and snapshots the rendered manifests;
//! 3. validates the rendered manifests with `kubeconform -strict` against the
//!    Kubernetes schemas and the datreeio CRDs-catalog (CiliumNetworkPolicy).
//!
//! Snapshots live in `tests/snapshots/` and are reviewed as YAML diffs.
//! Update them with `just snapshots` (see CONTRIBUTING.md).
//!
//! `helm` and `kubeconform` are optional locally: when missing, steps 2 and 3
//! are skipped with a message. Set `TONIN_REQUIRE_HELM=1` (CI does) to make a
//! missing tool a failure. `TONIN_TEST_HELM` / `TONIN_TEST_KUBECONFORM`
//! override the binaries used (default: `helm` / `kubeconform` on PATH).
//! Snapshots are recorded with the helm version pinned in
//! `.github/workflows/ci.yml`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environments rendered for every fixture.
const ENVS: &[&str] = &["dev", "prd"];

/// Kubernetes version the schemas (and `helm template --kube-version`) target.
const KUBE_VERSION: &str = "1.34.0";

/// CRD schemas (CiliumNetworkPolicy, …) from the datreeio CRDs-catalog.
const CRD_SCHEMA_LOCATION: &str = "https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json";

/// Rendered resources that fail kubeconform today, as
/// `(fixture, env, kind, name, reason)`. Each entry must still fail (a fixed
/// resource has to be removed from this list), and nothing else may fail.
const KNOWN_INVALID: &[(&str, &str, &str, &str, &str)] = &[
    // TODO(tonin 0.15 T2): T2 stops rendering Postgres/Redis StatefulSets.
    // db-/cache-statefulset.yaml set `app: <db|cache name>` and then include
    // `service.labels`, which sets `app` again: duplicate YAML key.
    (
        "grpc-stateful",
        "prd",
        "StatefulSet",
        "identity-db",
        "duplicate `app` label key",
    ),
    (
        "grpc-stateful",
        "prd",
        "StatefulSet",
        "identity-cache",
        "duplicate `app` label key",
    ),
    (
        "grpc-http-cilium",
        "prd",
        "StatefulSet",
        "telemetry-db",
        "duplicate `app` label key",
    ),
    (
        "grpc-http-cilium",
        "prd",
        "StatefulSet",
        "telemetry-cache",
        "duplicate `app` label key",
    ),
];

macro_rules! golden_fixture {
    ($test:ident, $fixture:literal) => {
        #[test]
        fn $test() {
            run_fixture($fixture);
        }
    };
}

golden_fixture!(grpc_stateful, "grpc-stateful");
golden_fixture!(http_api, "http-api");
golden_fixture!(grpc_http_cilium, "grpc-http-cilium");
golden_fixture!(node_web, "node-web");

fn fixture_dir(fixture: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/charts")
        .join(fixture)
}

fn run_fixture(fixture: &str) {
    let service_dir = fixture_dir(fixture);
    let out = tempfile::tempdir().expect("tempdir");
    let chart = out.path().join("chart");
    generate_chart(&service_dir, &chart);

    for file in [
        "Chart.yaml",
        "values.yaml",
        "values-dev.yaml",
        "values-prd.yaml",
    ] {
        insta::assert_snapshot!(format!("{fixture}__{file}"), read(&chart.join(file)));
    }

    let Some(helm) = tool("helm", "TONIN_TEST_HELM", "version") else {
        return;
    };
    let kubeconform = tool("kubeconform", "TONIN_TEST_KUBECONFORM", "-v");

    for env in ENVS {
        let rendered = helm_template(&helm, &service_dir, &chart, env);
        insta::assert_snapshot!(format!("{fixture}__{env}__rendered"), rendered);

        if let Some(kubeconform) = &kubeconform {
            let manifest = out.path().join(format!("rendered-{env}.yaml"));
            std::fs::write(&manifest, &rendered).expect("write rendered manifest");
            check_schemas(kubeconform, fixture, env, &manifest);
        }
    }
}

/// Run `tonin helm generate` in a clean environment (no ambient `TONIN_*`).
fn generate_chart(service_dir: &Path, chart: &Path) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tonin"));
    cmd.args(["helm", "generate", "--path"])
        .arg(service_dir)
        .arg("--out")
        .arg(chart);
    for env in ENVS {
        cmd.args(["--env", env]);
    }
    for var in ["TONIN_ENV", "TONIN_IMAGE_PREFIX", "TONIN_SERVICE_DIR"] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn tonin");
    assert!(
        output.status.success(),
        "tonin helm generate failed for {}:\n{}",
        service_dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `helm template`, release name and namespace resolved like `tonin helm template`.
fn helm_template(helm: &Path, service_dir: &Path, chart: &Path, env: &str) -> String {
    let plan = tonin_plugin::Plan::load_with_env(&service_dir.join("tonin.toml"), env)
        .unwrap_or_else(|e| panic!("loading plan for env {env}: {e}"));
    let output = Command::new(helm)
        .args(["template", &plan.name])
        .arg(chart)
        .arg("-f")
        .arg(chart.join("values.yaml"))
        .arg("-f")
        .arg(chart.join(format!("values-{env}.yaml")))
        .args(["--namespace", &plan.namespace])
        .args(["--kube-version", KUBE_VERSION])
        .output()
        .expect("spawn helm");
    assert!(
        output.status.success(),
        "helm template failed ({env}):\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("helm output is UTF-8")
}

/// Validate with kubeconform and compare the failures with `KNOWN_INVALID`.
fn check_schemas(kubeconform: &Path, fixture: &str, env: &str, manifest: &Path) {
    // One schema cache per test keeps parallel runs from sharing files.
    let cache = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("kubeconform-cache")
        .join(format!("{fixture}-{env}"));
    std::fs::create_dir_all(&cache).expect("create kubeconform cache dir");

    let output = Command::new(kubeconform)
        .args(["-strict", "-output", "json"])
        .args(["-kubernetes-version", KUBE_VERSION])
        .arg("-cache")
        .arg(&cache)
        .args(["-schema-location", "default"])
        .args(["-schema-location", CRD_SCHEMA_LOCATION])
        .arg(manifest)
        .output()
        .expect("spawn kubeconform");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "kubeconform output is not JSON ({e}):\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });

    let mut failing = BTreeSet::new();
    let mut details = Vec::new();
    for res in report["resources"].as_array().into_iter().flatten() {
        let status = res["status"].as_str().unwrap_or_default();
        if status == "statusValid" || status == "statusSkipped" {
            continue;
        }
        let kind = res["kind"].as_str().unwrap_or_default().to_string();
        let name = res["name"].as_str().unwrap_or_default().to_string();
        details.push(format!("  {kind}/{name} ({status}): {}", res["msg"]));
        failing.insert((kind, name));
    }

    let expected: BTreeSet<(String, String)> = KNOWN_INVALID
        .iter()
        .filter(|(f, e, ..)| *f == fixture && *e == env)
        .map(|(_, _, kind, name, _)| (kind.to_string(), name.to_string()))
        .collect();

    let unexpected: Vec<_> = failing.difference(&expected).collect();
    let fixed: Vec<_> = expected.difference(&failing).collect();
    assert!(
        unexpected.is_empty() && fixed.is_empty(),
        "kubeconform results for {fixture}/{env} differ from KNOWN_INVALID.\n\
         newly failing: {unexpected:?}\n\
         now valid (remove from KNOWN_INVALID): {fixed:?}\n\
         kubeconform failures:\n{}\n\
         (schema download errors need network access to raw.githubusercontent.com)",
        details.join("\n")
    );
}

/// Locate a CLI tool. `None` (with a message) when it is missing and
/// `TONIN_REQUIRE_HELM` is unset; a panic when it is set.
fn tool(name: &str, override_var: &str, version_arg: &str) -> Option<PathBuf> {
    let bin = std::env::var_os(override_var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(name));
    let found = Command::new(&bin)
        .arg(version_arg)
        .output()
        .is_ok_and(|o| o.status.success());
    if found {
        return Some(bin);
    }
    let required = std::env::var("TONIN_REQUIRE_HELM").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(
        !required,
        "`{}` not found but TONIN_REQUIRE_HELM is set; install {name} (see CONTRIBUTING.md)",
        bin.display()
    );
    eprintln!(
        "SKIPPED: `{}` not found; golden {name} checks skipped (set TONIN_REQUIRE_HELM=1 to require them)",
        bin.display()
    );
    None
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}
