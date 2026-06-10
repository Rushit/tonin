//! `tonin k8s ...` — render, validate, diff, apply, setup.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::codegen::{plan::Plan, render};
use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum K8sCmd {
    /// Render YAML manifests from tonin.toml.
    ///
    /// Parses `[service]`, `[deploy]`, `[resources]`, and any capability
    /// blocks (`[database]`, `[databases.*]`, `[cache]`, `[caches.*]`,
    /// `[callers]`, `[secrets]`, `[migrations]`, `[config]`) and writes
    /// Deployment / Service / HPA / Ingress + mesh overlays into ./k8s/
    /// (or `--out`). Offline — no cluster contact required.
    ///
    /// Each block supports per-env overlay subtables. `[deploy.dev]`,
    /// `[database.dev]`, `[cache.dev]`, and `[callers.dev]` override only
    /// the fields they specify for that environment; unset fields fall back
    /// to the top-level value.
    #[command(after_long_help = "PREREQUISITES:
  - tonin.toml in the current dir (or `--path`).

SIDE EFFECTS:
  Writes files under ./<out>/ (default ./k8s/). Overwrites existing
  files in that directory. Keep dev and prod manifests separate with --out:
    tonin k8s generate --env dev  --out k8s/dev
    tonin k8s generate --env prod --out k8s/prod

EXAMPLES:
  # Render prod manifests into k8s/prod/
  tonin k8s generate --env prod --out k8s/prod

  # Render dev manifests into k8s/dev/ (shared DB + Redis, 1 replica)
  tonin k8s generate --env dev --out k8s/dev

  # Preview dev to stdout without writing files
  tonin k8s generate --env dev --dry-run

  # Render every service in a workspace
  tonin k8s generate --workspace --path ./services --env prod --out k8s/prod

TONIN.TOML — PER-ENV OVERLAY EXAMPLES:

  # [deploy] — replicas and namespace per env
  [deploy]
  replicas  = 2
  namespace = \"agnitiv\"

  [deploy.dev]
  replicas  = 1
  namespace = \"agnitiv-dev\"

  # [callers] — ingress allowlist per env (supports per-env subtable)
  [callers]
  gateway         = \"agnitiv\"
  zradar-platform = \"agnitiv\"

  [callers.dev]
  gateway         = \"agnitiv-dev\"
  zradar-platform = \"agnitiv-dev\"

  # [database] — primary DB: owned in prod, shared in dev with literal URL
  [database]
  engine = \"postgres\"

  [database.dev]
  shared = true
  url    = \"postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/identity_dev\"

  # [databases.*] — named DBs: emit <NAME>_DATABASE_URL / <NAME>_DATABASE_PASSWORD
  # dev may use one shared write DB; prod may have separate read replica.
  [databases.write]
  engine = \"postgres\"

  [databases.write.dev]
  shared = true
  url    = \"postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/identity_dev\"

  [databases.read]
  engine = \"postgres\"
  shared = true
  name   = \"identity-read-replica\"
  namespace = \"agnitiv\"

  [databases.read.dev]
  shared = true
  url    = \"postgresql://postgres:postgres@postgres.shared-dev.svc.cluster.local:5432/identity_dev\"

  # [cache] — primary cache
  [cache]
  engine = \"redis\"

  [cache.dev]
  shared = true
  url    = \"redis://redis.shared-dev.svc.cluster.local:6379\"

  # [caches.*] — named caches: emit <NAME>_REDIS_URL
  [caches.session]
  engine = \"redis\"

  [caches.session.dev]
  shared = true
  url    = \"redis://redis.shared-dev.svc.cluster.local:6379\"

SEE ALSO:
  docs/12-kubernetes-deploy.md")]
    Generate(GenerateArgs),
    /// Validate generated YAML against the cluster via server-side dry-run.
    ///
    /// Renders the same YAML as `generate` and submits it via
    /// `kubectl apply --dry-run=server`. The cluster's admission
    /// controllers and validators run; nothing is created.
    #[command(after_long_help = "PREREQUISITES:
  - kubectl on PATH
  - A reachable Kubernetes cluster
  - Current kubectl context points where you want to validate
  - tonin.toml in the current dir

EXAMPLES:
  tonin k8s validate
  tonin k8s validate --env staging

EXIT CODES:
  0 — every manifest accepted by the apiserver
  non-zero — at least one manifest rejected; stderr carries the reason")]
    Validate(GenerateArgs),
    /// Show what would change if the rendered YAML were applied.
    ///
    /// Wraps `kubectl diff -f`. The diff is computed against the cluster's
    /// current view of the same objects (per `kubectl diff` semantics).
    #[command(after_long_help = "PREREQUISITES:
  - kubectl on PATH (the `diff` subcommand specifically)
  - A reachable Kubernetes cluster
  - tonin.toml in the current dir

EXAMPLES:
  tonin k8s diff
  tonin k8s diff --workspace --path ./services

EXIT CODES:
  0 — no differences (cluster matches the would-be-applied state)
  1 — differences exist (per kubectl diff semantics)
  >1 — error")]
    Diff(GenerateArgs),
    /// Render YAML and apply it to the current kubectl context.
    ///
    /// Equivalent to `generate` followed by `kubectl apply -f` against
    /// every rendered file. Use `--context` to override the current
    /// kubectl context. **This actually creates / updates resources in
    /// the cluster.**
    #[command(after_long_help = "PREREQUISITES:
  - kubectl on PATH
  - A reachable Kubernetes cluster
  - Current kubectl context points to your target cluster (unless
    overridden with `--context`)
  - tonin.toml in the current dir
  - For workspace mode, every tonin.toml under `--path` must be valid

SIDE EFFECTS:
  - Writes files under ./<out>/ (default ./k8s/)
  - Calls `kubectl apply` — creates / mutates real resources in the
    target cluster. There is no rollback; review the manifests first
    or pair with `k8s diff`.

EXAMPLES:
  # Apply to whatever the current kubectl context points at
  tonin k8s apply

  # Apply to a named context (e.g. a staging cluster)
  tonin k8s apply --context staging

  # Apply every service in a workspace
  tonin k8s apply --workspace --path ./services")]
    Apply(ApplyArgs),
    /// Set up a target cluster for tonin (OTel collector, mesh checks).
    ///
    /// Stub today — prints the manual install commands that match
    /// `[deploy].mesh` and the OTel collector defaults. The intent is a
    /// one-shot post-`kind create cluster` setup; full implementation
    /// lands when the mesh-install matrix is settled.
    #[command(after_long_help = "MODES:
  --mode local   For kind / k3d / minikube. Installs in-cluster defaults
                 (OTel collector). Assumes the cluster is local-dev disposable.
  --mode remote  Uses the current kubectl context as-is. Prints commands
                 rather than running them when destructive.

EXAMPLES:
  tonin k8s setup --mode local
  tonin k8s setup --mode remote")]
    Setup(SetupArgs),
}

#[derive(clap::Args, Clone)]
pub struct GenerateArgs {
    /// Service directory containing tonin.toml. Defaults to current dir.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
    /// Render every tonin.toml under `path` recursively.
    /// Cross-service `[depends_on]` graph is computed across all services found.
    #[arg(long)]
    pub workspace: bool,
    /// Output directory (relative to each service dir).
    #[arg(long, default_value = "k8s")]
    pub out: PathBuf,
    /// Print to stdout instead of writing files.
    #[arg(long)]
    pub dry_run: bool,
    /// Target environment for overlay resolution.
    /// Applies to: `[deploy.<env>]`, `[callers.<env>]`, `[database.<env>]`,
    /// `[databases.*.<env>]`, `[cache.<env>]`, `[caches.*.<env>]`.
    /// Falls back to `TONIN_ENV` env var, then `dev`.
    #[arg(long)]
    pub env: Option<String>,
}

#[derive(clap::Args)]
pub struct ApplyArgs {
    #[command(flatten)]
    pub generate: GenerateArgs,
    /// Override the kubectl context.
    #[arg(long)]
    pub context: Option<String>,
}

#[derive(clap::Args)]
pub struct SetupArgs {
    /// `local` = kind/k3d/minikube (defaults installed in-cluster).
    /// `remote` = uses current kubectl context; expects credentials already configured.
    #[arg(long, default_value = "local")]
    pub mode: String,
}

pub fn run(cmd: K8sCmd) -> Result<()> {
    match cmd {
        K8sCmd::Generate(args) => generate(&args),
        K8sCmd::Validate(args) => validate(&args),
        K8sCmd::Diff(args) => diff(&args),
        K8sCmd::Apply(args) => apply(&args),
        K8sCmd::Setup(args) => setup(&args),
    }
}

// --------------- generate ---------------

fn generate(args: &GenerateArgs) -> Result<()> {
    let plans = load_plans(&args.path, args.workspace, args.env.as_deref())?;
    for plan in &plans {
        let files = render::render(plan).context("rendering plan")?;
        let target_dir = plan.dir.join(&args.out);

        if args.dry_run {
            println!("# === {} ({} files) ===", plan.name, files.len());
            for f in &files {
                println!("# --- {}/{} ---", target_dir.display(), f.path);
                println!("{}", f.contents);
            }
        } else {
            std::fs::create_dir_all(&target_dir)
                .with_context(|| format!("creating {}", target_dir.display()))?;
            for f in &files {
                let p = target_dir.join(&f.path);
                std::fs::write(&p, &f.contents)
                    .with_context(|| format!("writing {}", p.display()))?;
            }
            eprintln!(
                "wrote {} files for service '{}' → {}",
                files.len(),
                plan.name,
                target_dir.display()
            );

            // Ensure any secret manifests written to the k8s dir are
            // excluded from git — fulfils the promise in db-secret.yaml.tmpl
            // and secrets.yaml.tmpl ("added to .gitignore by tonin").
            let secret_paths: Vec<String> = files
                .iter()
                .filter(|f| is_secret_manifest(&f.path))
                .map(|f| format!("{}/{}", args.out.display(), f.path))
                .collect();
            if !secret_paths.is_empty() {
                protect_gitignore(&plan.dir, &secret_paths)
                    .with_context(|| format!("updating .gitignore for '{}'", plan.name))?;
            }
        }
    }
    Ok(())
}

/// Returns true for manifest files whose contents must never be committed.
fn is_secret_manifest(path: &str) -> bool {
    matches!(path, "secrets.yaml" | "db-secret.yaml")
}

/// Append `entries` to the service dir's `.gitignore`, skipping any that
/// are already present. Creates the file if it does not exist.
fn protect_gitignore(service_dir: &Path, entries: &[String]) -> Result<()> {
    let gitignore = service_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();

    let new_entries: Vec<&str> = entries
        .iter()
        .map(String::as_str)
        .filter(|e| !existing.lines().any(|l| l.trim() == *e))
        .collect();

    if new_entries.is_empty() {
        return Ok(());
    }

    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("\n# tonin: generated secret manifests — never commit plaintext values\n");
    for entry in new_entries {
        content.push_str(entry);
        content.push('\n');
    }

    std::fs::write(&gitignore, content)
        .with_context(|| format!("writing {}", gitignore.display()))?;

    eprintln!("  updated .gitignore ← {}", entries.join(", "));
    Ok(())
}

// --------------- validate ---------------

fn validate(args: &GenerateArgs) -> Result<()> {
    require_kubectl()?;
    let plans = load_plans(&args.path, args.workspace, args.env.as_deref())?;
    let tmp = tempfile::tempdir().context("creating temp dir")?;
    let mut total = 0;
    let mut fail = 0;

    for plan in &plans {
        let svc_dir = tmp.path().join(&plan.name);
        std::fs::create_dir_all(&svc_dir)?;
        for f in render::render(plan)? {
            std::fs::write(svc_dir.join(&f.path), f.contents)?;
            total += 1;
        }
        let status = Command::new("kubectl")
            .args(["apply", "--dry-run=server", "-f"])
            .arg(&svc_dir)
            .status()
            .context("running kubectl apply --dry-run=server")?;
        if status.success() {
            eprintln!("✓ {}: valid", plan.name);
        } else {
            eprintln!("✗ {}: kubectl reported errors above", plan.name);
            fail += 1;
        }
    }
    eprintln!(
        "validated {} files across {} services; {} failed",
        total,
        plans.len(),
        fail
    );
    if fail > 0 {
        bail!("{} service(s) failed validation", fail);
    }
    Ok(())
}

// --------------- diff ---------------

fn diff(args: &GenerateArgs) -> Result<()> {
    require_kubectl()?;
    let plans = load_plans(&args.path, args.workspace, args.env.as_deref())?;
    let tmp = tempfile::tempdir().context("creating temp dir")?;
    for plan in &plans {
        let svc_dir = tmp.path().join(&plan.name);
        std::fs::create_dir_all(&svc_dir)?;
        for f in render::render(plan)? {
            std::fs::write(svc_dir.join(&f.path), f.contents)?;
        }
        eprintln!("# diff: {}", plan.name);
        // kubectl diff exits 1 when there is a diff — that's not an error for us.
        Command::new("kubectl")
            .args(["diff", "-f"])
            .arg(&svc_dir)
            .status()
            .context("running kubectl diff")?;
    }
    Ok(())
}

// --------------- apply ---------------

fn apply(args: &ApplyArgs) -> Result<()> {
    require_kubectl()?;
    // Render to the user's chosen output dir (default ./k8s/) so the YAML
    // is inspectable after, not buried in a tempdir.
    generate(&args.generate)?;

    let plans = load_plans(
        &args.generate.path,
        args.generate.workspace,
        args.generate.env.as_deref(),
    )?;
    for plan in &plans {
        let target_dir = plan.dir.join(&args.generate.out);
        let mut cmd = Command::new("kubectl");
        if let Some(ctx) = &args.context {
            cmd.args(["--context", ctx]);
        }
        cmd.args(["apply", "-f"]).arg(&target_dir);
        let status = cmd.status().context("running kubectl apply")?;
        if !status.success() {
            bail!("kubectl apply failed for service '{}'", plan.name);
        }
        eprintln!("✓ {}: applied", plan.name);
    }
    Ok(())
}

// --------------- setup ---------------

fn setup(args: &SetupArgs) -> Result<()> {
    // Intentionally stubbed. The setup flow needs a real design pass:
    //
    // local mode:
    //   - detect kind/k3d/minikube, fail with install hint if none.
    //   - install Cilium with `--set encryption.enabled=true` (or print helm cmd).
    //   - install OTel collector from templates/k8s/otel-collector.yaml.tmpl.
    //   - verify `linkerd check` / `cilium status` equivalent.
    //
    // remote mode:
    //   - use current kubectl context; do not modify credentials.
    //   - check the cluster has the chosen mesh CNI installed; refuse to proceed otherwise.
    //   - apply OTel collector to `observability` ns if missing.
    //
    // Auth: we should NOT bake in cloud-provider auth (gcloud/aws/az). Users own kubectl
    // context setup. We only call kubectl with --context if provided.
    eprintln!("tonin k8s setup --mode={} : not yet implemented", args.mode);
    eprintln!("manual setup for now:");
    eprintln!("  1. Create cluster (kind/k3d/EKS/GKE/AKS)");
    eprintln!("  2. Install Cilium (or chosen mesh) per docs/mesh-setup.md");
    eprintln!("  3. kubectl apply -f templates/k8s/otel-collector.yaml.tmpl");
    eprintln!("  4. tonin k8s validate --workspace --path examples");
    Ok(())
}

// --------------- helpers ---------------

fn load_plans(path: &Path, workspace: bool, env: Option<&str>) -> Result<Vec<Plan>> {
    check_cli_version();
    let env = crate::codegen::stateful::select_env(env);
    let plans = if workspace {
        Plan::load_workspace_with_env(path, &env).context("loading workspace plans")?
    } else {
        let toml = path.join("tonin.toml");
        if !toml.exists() {
            return Err(anyhow!(
                "no tonin.toml at {}; pass --workspace to scan recursively",
                toml.display()
            ));
        }
        vec![Plan::load_with_env(&toml, &env).context("loading plan")?]
    };
    if plans.is_empty() {
        bail!("no services found under {}", path.display());
    }
    Ok(plans)
}

/// Warn (never error) when the CLI is older than the minimum version
/// recommended by the version of `tonin-plugin` compiled in.
///
/// This catches the common case of a service repo upgrading `tonin-plugin`
/// (e.g. to pick up a new `[eventbus]` section) while the developer's
/// installed CLI binary is still the previous version and would silently
/// ignore the new fields.
fn check_cli_version() {
    use crate::codegen::plan::RECOMMENDED_CLI_MIN;
    let cli_ver = env!("CARGO_PKG_VERSION");
    if version_lt(cli_ver, RECOMMENDED_CLI_MIN) {
        eprintln!(
            "warning: tonin {cli_ver} is older than the minimum recommended \
             by this project's tonin-plugin ({RECOMMENDED_CLI_MIN}). \
             Some tonin.toml fields may be silently ignored. \
             Run `cargo install tonin` to upgrade."
        );
    }
}

/// Returns true when `a` is strictly less than `b` using semver ordering.
/// Falls back to false on any parse error so the check never blocks a build.
fn version_lt(a: &str, b: &str) -> bool {
    fn parse(s: &str) -> Option<(u32, u32, u32)> {
        let mut it = s.split('.');
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        ))
    }
    match (parse(a), parse(b)) {
        (Some(av), Some(bv)) => av < bv,
        _ => false,
    }
}

fn require_kubectl() -> Result<()> {
    Command::new("kubectl")
        .arg("version")
        .arg("--client")
        .output()
        .map_err(|e| anyhow!("kubectl not found on PATH: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::version_lt;

    #[test]
    fn version_lt_works() {
        assert!(version_lt("0.4.1", "0.4.2"));
        assert!(version_lt("0.3.9", "0.4.0"));
        assert!(!version_lt("0.4.2", "0.4.2"));
        assert!(!version_lt("0.5.0", "0.4.2"));
        assert!(!version_lt("bad", "0.4.2"));
    }
}
