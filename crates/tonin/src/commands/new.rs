//! `tonin service new <name> --lang <rust|python|ts> [--type web|backend]`.
//!
//! Scaffold templates use plain `{{ var }}` substitution — no loops, no
//! conditionals. Done with `str::replace` (not a template engine) so the
//! syntax can't collide with TSX/JS object literals or anything else inside
//! the templates. The k8s renderer is a separate pipeline that does use
//! Tera, because cross-service network policies need real loops.

use std::path::{Path, PathBuf};

use crate::codegen::{plan::Plan, render};
use anyhow::{Context, Result, anyhow, bail};
use convert_case::{Case, Casing};
use include_dir::{Dir, include_dir};

use super::service::{ClientLang, Lang, ServiceType, StorageKind, WebMode};

static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates/service");

struct Vars {
    service_name: String,
    /// Snake-case form, suitable for Rust `use` paths and proto package
    /// names. `service-name` → `service_name`. Uses **heck**'s
    /// snake-case algorithm — which is the same one prost uses
    /// internally to generate module paths from proto `service` names.
    /// Matters for names with embedded digits: `s3svc` → `s3svc` (heck),
    /// not `s_3_svc` (convert_case::Snake).
    service_name_snake: String,
    /// Pascal-case form (`MyService`), used as the proto `service Name`
    /// and as the prefix of the generated client struct (`NameClient`).
    service_camel: String,
    /// Module path prost generates from the Pascal-case service name.
    /// For `MyService` → `my_service` (so the client lives at
    /// `proto::my_service_client::MyServiceClient`). Same heck snake
    /// algorithm as above.
    service_proto_module: String,
    language: String,
    service_type: String,
}

impl Vars {
    fn apply(&self, src: &str) -> String {
        src.replace("{{ service_proto_module }}", &self.service_proto_module)
            .replace("{{service_proto_module}}", &self.service_proto_module)
            .replace("{{ service_name_snake }}", &self.service_name_snake)
            .replace("{{service_name_snake}}", &self.service_name_snake)
            .replace("{{ service_name }}", &self.service_name)
            .replace("{{service_name}}", &self.service_name)
            .replace("{{ ServiceCamel }}", &self.service_camel)
            .replace("{{ServiceCamel}}", &self.service_camel)
            .replace("{{ language }}", &self.language)
            .replace("{{language}}", &self.language)
            .replace("{{ service_type }}", &self.service_type)
            .replace("{{service_type}}", &self.service_type)
    }
}

#[allow(clippy::too_many_arguments)] // each arg maps to a `tonin service new` CLI flag; bundling into a struct buys nothing here
pub fn run(
    name: &str,
    lang: Lang,
    st: ServiceType,
    wm: Option<WebMode>,
    no_workspace: bool,
    with_jobs: &[String],
    with_storage: Option<StorageKind>,
    extra_clients: &[ClientLang],
) -> Result<()> {
    validate_name(name)?;
    for job in with_jobs {
        validate_name(job).with_context(|| format!("invalid --with-job name '{job}'"))?;
    }

    let dest = PathBuf::from(name);
    if dest.exists() {
        bail!(
            "{} already exists; pick another name or delete it first",
            dest.display()
        );
    }

    use heck::ToSnakeCase as _;
    let service_camel = name.to_case(Case::Pascal);
    let vars = Vars {
        service_name: name.to_string(),
        // heck::to_snake_case matches prost's internal snake-casing of
        // proto identifiers — so the proto package name we write here
        // matches the module path prost generates downstream.
        service_name_snake: name.to_snake_case(),
        service_proto_module: service_camel.to_snake_case(),
        service_camel,
        language: lang.as_str().to_string(),
        service_type: st.as_str().to_string(),
    };

    scaffold(&dest, lang, st, wm, &vars)?;
    write_service_toml(&dest, &vars, st, wm)?;
    if let Some(kind) = with_storage {
        emit_storage_block_in_micro_toml(&dest, kind)?;
    }
    if !with_jobs.is_empty() {
        match lang {
            Lang::Rust => emit_jobs(&dest, &vars, with_jobs)?,
            Lang::Python => emit_jobs_python(&dest, &vars, with_jobs)?,
            Lang::Ts => unreachable!("--with-job rejected for ts in service::run"),
        }
    }
    if let Some(kind) = with_storage {
        match lang {
            Lang::Rust => emit_storage_rust(&dest, &vars, kind)?,
            Lang::Python => emit_storage_python(&dest, &vars, kind)?,
            Lang::Ts => unreachable!("--with-storage rejected for ts in service::run"),
        }
    }
    for extra in extra_clients {
        emit_extra_client(&dest, &vars, *extra)?;
    }
    pregenerate_k8s(&dest)?;
    emit_claude_md(&dest, name, lang, st, wm)?;
    emit_gitignore(&dest)?;
    emit_docs_tree(&dest, name)?;

    if matches!(lang, Lang::Rust) && !no_workspace {
        maybe_add_to_workspace(&dest)?;
    }

    print_next_steps(name, lang, st, wm, with_jobs, with_storage, extra_clients);
    Ok(())
}

/// Append a `[storage]` block to the freshly-emitted tonin.toml so k8s
/// rendering + service-author docs reflect the chosen backend.
fn emit_storage_block_in_micro_toml(dest: &Path, kind: StorageKind) -> Result<()> {
    let path = dest.join("tonin.toml");
    let mut content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(
        "\n# Object storage. Secrets come from STORAGE_* env (see\n\
         # server/src/storage.rs or jobs/<name>.py for the full list).\n\
         [storage]\n\
         kind = \"{kind_str}\"\n",
        kind_str = kind.as_str()
    ));
    std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Emit `server/src/storage.rs` + add `opendal` dep + wire into main.rs.
/// The trait lives in tonin-core; this file is the user-owned default
/// impl. Users can hand-edit or replace it with their own
/// `StorageProvider` implementation without breaking the framework
/// contract.
fn emit_storage_rust(dest: &Path, vars: &Vars, kind: StorageKind) -> Result<()> {
    let storage_path = dest.join("server/src/storage.rs");
    std::fs::write(&storage_path, render_storage_rs_rust(kind))
        .with_context(|| format!("writing {}", storage_path.display()))?;

    // Add `pub mod storage;` to the server lib.rs.
    let lib_path = dest.join("server/src/lib.rs");
    let mut lib = std::fs::read_to_string(&lib_path)
        .with_context(|| format!("reading {}", lib_path.display()))?;
    if !lib.contains("pub mod storage") {
        // Insert after the existing `pub mod auth;` line if present.
        if lib.contains("pub mod auth;") {
            let insertion = "pub mod auth;\npub mod storage;";
            lib = lib.replacen("pub mod auth;", insertion, 1);
        } else {
            lib.push_str("\npub mod storage;\n");
        }
        std::fs::write(&lib_path, &lib)?;
    }

    // Add opendal dep with the matching feature to server/Cargo.toml.
    let cargo_path = dest.join("server/Cargo.toml");
    let mut cargo = std::fs::read_to_string(&cargo_path)
        .with_context(|| format!("reading {}", cargo_path.display()))?;
    let dep_line = format!(
        "opendal = {{ version = \"0.57\", default-features = false, features = [\"{feat}\"] }}\n",
        feat = kind.opendal_feature()
    );
    if !cargo.contains("opendal") {
        // Insert just after the `[dependencies]` header. Falls back to
        // appending at end if the section can't be found (shouldn't
        // happen with our templates).
        if let Some(idx) = cargo.find("[dependencies]") {
            // Move past the line.
            let after_header = idx + "[dependencies]".len();
            // Find next newline.
            let nl = cargo[after_header..]
                .find('\n')
                .map(|n| after_header + n + 1)
                .unwrap_or(after_header);
            cargo.insert_str(nl, &dep_line);
        } else {
            cargo.push_str(&dep_line);
        }
        std::fs::write(&cargo_path, &cargo)?;
    }

    // Wire State::with_storage(...) into main.rs.
    let main_path = dest.join("server/src/main.rs");
    let mut main_src = std::fs::read_to_string(&main_path)
        .with_context(|| format!("reading {}", main_path.display()))?;
    let needle = "let state = State::from_env().await?;";
    let replacement = format!(
        "let state = State::from_env().await?\n        \
         .with_storage({snake}_server::storage::OpendalStorage::from_env().await?)\n        \
         .await?;",
        snake = vars.service_name_snake
    );
    if main_src.contains(needle) {
        main_src = main_src.replacen(needle, &replacement, 1);
        std::fs::write(&main_path, &main_src)?;
    }

    eprintln!(
        "✓ emitted Rust storage wiring ({kind})",
        kind = kind.as_str()
    );
    Ok(())
}

/// Python equivalent: emit `<svc>_server/storage.py` + add opendal to
/// pyproject.toml + wire into the server bootstrap. (The Python
/// scaffold restructure to 3-folder lives in a follow-up task; for
/// now we touch what exists.)
/// Emit `server/src/<svc>_server/storage.py`, add `opendal` to the
/// server's pyproject `[project.dependencies]`, and wire
/// `state.with_storage(...)` into `main.py`. Mirrors `emit_storage_rust`
/// for the 3-folder Python layout.
fn emit_storage_python(dest: &Path, vars: &Vars, kind: StorageKind) -> Result<()> {
    let snake = &vars.service_name_snake;
    let pkg_dir = dest.join("server/src").join(format!("{snake}_server"));
    std::fs::create_dir_all(&pkg_dir).with_context(|| format!("creating {}", pkg_dir.display()))?;

    let storage_py_path = pkg_dir.join("storage.py");
    std::fs::write(&storage_py_path, render_storage_py(kind))
        .with_context(|| format!("writing {}", storage_py_path.display()))?;

    // Add `opendal>=0.45` to server/pyproject.toml's [project.dependencies]
    // array. The scaffold pyproject lists deps with one item per line in
    // a multi-line array — we splice in a new line before the closing
    // bracket of that array.
    let pyproject_path = dest.join("server/pyproject.toml");
    let mut text = std::fs::read_to_string(&pyproject_path)
        .with_context(|| format!("reading {}", pyproject_path.display()))?;
    if !text.contains("opendal") {
        // Find `dependencies = [` then the matching `]`.
        if let Some(start) = text.find("dependencies = [") {
            let from = start + "dependencies = [".len();
            if let Some(rel_end) = text[from..].find(']') {
                let end = from + rel_end;
                text.insert_str(end, "  \"opendal>=0.45\",\n");
                std::fs::write(&pyproject_path, &text)?;
            }
        }
    }

    // Wire `state = (await State.from_env()).with_storage(...)` into
    // main.py. Replace the existing `state = await tonin.State.from_env()`
    // line with the chained version.
    let main_path = pkg_dir.join("main.py");
    let mut main_src = std::fs::read_to_string(&main_path)
        .with_context(|| format!("reading {}", main_path.display()))?;
    let needle = "state = await tonin.State.from_env()";
    let replacement = format!(
        "state = await tonin.State.from_env()\n    \
         from {snake}_server.storage import OpendalStorage\n    \
         state = await state.with_storage(await OpendalStorage.from_env())",
    );
    if main_src.contains(needle) {
        main_src = main_src.replacen(needle, &replacement, 1);
        std::fs::write(&main_path, &main_src)?;
    }

    eprintln!(
        "✓ emitted Python storage module ({kind})",
        kind = kind.as_str()
    );
    Ok(())
}

/// Generate the `storage.rs` file the scaffold ships when --with-storage is used.
///
/// The trait lives in tonin-core; this is a concrete impl users own.
fn render_storage_rs_rust(kind: StorageKind) -> String {
    let kind_str = kind.as_str();
    let builder_block = match kind {
        StorageKind::S3 => {
            r#"
        // opendal 0.57 builder methods consume + return Self.
        let mut builder = opendal::services::S3::default()
            .bucket(&env("STORAGE_BUCKET")?);
        if let Ok(region) = std::env::var("STORAGE_REGION") {
            builder = builder.region(&region);
        }
        if let Ok(endpoint) = std::env::var("STORAGE_ENDPOINT") {
            builder = builder.endpoint(&endpoint);
        }
        if let Ok(ak) = std::env::var("STORAGE_ACCESS_KEY") {
            builder = builder.access_key_id(&ak);
        }
        if let Ok(sk) = std::env::var("STORAGE_SECRET_KEY") {
            builder = builder.secret_access_key(&sk);
        }
        let op = opendal::Operator::new(builder)
            .map_err(|e| Error::Config(format!("opendal S3 init: {e}")))?
            .finish();
"#
        }
        StorageKind::Gcs => {
            r#"
        let mut builder = opendal::services::Gcs::default()
            .bucket(&env("STORAGE_BUCKET")?);
        if let Ok(cred) = std::env::var("STORAGE_CREDENTIAL_PATH") {
            builder = builder.credential_path(&cred);
        }
        let op = opendal::Operator::new(builder)
            .map_err(|e| Error::Config(format!("opendal GCS init: {e}")))?
            .finish();
"#
        }
        StorageKind::Azure => {
            r#"
        let mut builder = opendal::services::Azblob::default()
            .container(&env("STORAGE_CONTAINER")?);
        if let Ok(acc) = std::env::var("STORAGE_ACCOUNT") {
            builder = builder.account_name(&acc);
        }
        if let Ok(key) = std::env::var("STORAGE_ACCESS_KEY") {
            builder = builder.account_key(&key);
        }
        let op = opendal::Operator::new(builder)
            .map_err(|e| Error::Config(format!("opendal Azblob init: {e}")))?
            .finish();
"#
        }
        StorageKind::Local => {
            r#"
        let builder = opendal::services::Fs::default()
            .root(&env("STORAGE_ROOT")?);
        let op = opendal::Operator::new(builder)
            .map_err(|e| Error::Config(format!("opendal Fs init: {e}")))?
            .finish();
"#
        }
    };

    format!(
        r#"//! Object-storage wiring (opendal-backed).
//!
//! Default `StorageProvider` impl for this service. The trait lives in
//! `tonin::state::StorageProvider`; everything below is owned by you
//! and safe to edit. To swap out opendal for a different SDK or to
//! layer in retries / logging, replace this file and update `main.rs`
//! to call `state.with_storage(YourImpl)` instead.
//!
//! Activation: scaffolded via `tonin service new --with-storage {kind_str}`.
//!
//! The boot probe runs `op.list("/").limit(1)` — cheap connectivity
//! check that catches missing bucket / wrong creds / network issues
//! before the service starts serving traffic.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use tonin::core::error::{{Error, Result}};
use tonin::core::state::StorageProvider;
use opendal::Operator;

/// Wraps an `opendal::Operator`. Cheap to clone (opendal Operator is
/// internally `Arc`-backed).
#[derive(Clone)]
pub struct OpendalStorage {{
    op: Operator,
}}

impl OpendalStorage {{
    /// Construct directly from a pre-built Operator. Use this when you
    /// want full control over the builder (e.g. layered retries).
    pub fn from_operator(op: Operator) -> Self {{
        Self {{ op }}
    }}

    /// Build from env (`STORAGE_*`). Probe is run later by
    /// `State::with_storage`.
    pub async fn from_env() -> Result<Self> {{
{builder_block}
        Ok(Self {{ op }})
    }}

    /// Borrow the underlying Operator for app code that wants the full
    /// opendal API (writes, reads, presigned URLs, etc.).
    pub fn operator(&self) -> &Operator {{
        &self.op
    }}
}}

#[async_trait]
impl StorageProvider for OpendalStorage {{
    async fn probe(&self) -> Result<()> {{
        // LIST limit 1 — cheap, no writes, verifies we can reach the bucket
        // and our creds work. Errors propagate as Config so the service
        // fails to start rather than 5xx-ing later.
        let mut lister = self.op.lister_with("/").limit(1).await
            .map_err(|e| Error::Config(format!("storage probe failed: {{e}}")))?;
        // Drain at most one entry, then stop. We don't care what's in
        // the bucket; we only care that the call succeeded.
        if let Some(item) = lister.next().await {{
            item.map_err(|e| Error::Config(format!("storage probe entry: {{e}}")))?;
        }}
        Ok(())
    }}

    fn system(&self) -> &'static str {{
        "{kind_str}"
    }}
}}

/// Convenience: bubble up a clear error when a required env var is missing.
fn env(name: &str) -> Result<String> {{
    std::env::var(name).map_err(|_| {{
        Error::Config(format!("{{name}} unset (required for --with-storage {kind_str})"))
    }})
}}

// Silence Arc<dyn StorageProvider> handle indirection when the user
// wants to share storage across spawned tasks.
#[allow(dead_code)]
fn _share_as_arc(s: OpendalStorage) -> Arc<dyn StorageProvider> {{
    Arc::new(s)
}}
"#,
    )
}

fn render_storage_py(kind: StorageKind) -> String {
    let kind_str = kind.as_str();
    let scheme = match kind {
        StorageKind::S3 => "s3",
        StorageKind::Gcs => "gcs",
        StorageKind::Azure => "azblob",
        StorageKind::Local => "fs",
    };
    format!(
        r#""""Object-storage wiring (opendal-backed).

Default storage helper for this service. Scaffolded via
``tonin service new --with-storage {kind_str}``. Owned by you — feel
free to swap opendal for a different SDK or add retry layers.

Boot probe: ``op.list("/", limit=1)`` — cheap connectivity check that
catches missing bucket / wrong creds before serving traffic.
"""
from __future__ import annotations

import logging
import os
from dataclasses import dataclass

logger = logging.getLogger(__name__)


def _env(name: str) -> str:
    v = os.environ.get(name)
    if not v:
        raise RuntimeError(f"{{name}} unset (required for --with-storage {kind_str})")
    return v


@dataclass(slots=True)
class OpendalStorage:
    """Wraps :class:`opendal.AsyncOperator`. All ops are awaited."""

    op: "object"  # opendal.AsyncOperator — typed as object to avoid hard import

    @classmethod
    async def from_env(cls) -> "OpendalStorage":
        import opendal

        kwargs: dict[str, str] = {{}}
        kind = "{scheme}"

        if kind == "s3":
            kwargs["bucket"] = _env("STORAGE_BUCKET")
            if (v := os.environ.get("STORAGE_REGION")):
                kwargs["region"] = v
            if (v := os.environ.get("STORAGE_ENDPOINT")):
                kwargs["endpoint"] = v
            if (v := os.environ.get("STORAGE_ACCESS_KEY")):
                kwargs["access_key_id"] = v
            if (v := os.environ.get("STORAGE_SECRET_KEY")):
                kwargs["secret_access_key"] = v
        elif kind == "gcs":
            kwargs["bucket"] = _env("STORAGE_BUCKET")
            if (v := os.environ.get("STORAGE_CREDENTIAL_PATH")):
                kwargs["credential_path"] = v
        elif kind == "azblob":
            kwargs["container"] = _env("STORAGE_CONTAINER")
            if (v := os.environ.get("STORAGE_ACCOUNT")):
                kwargs["account_name"] = v
            if (v := os.environ.get("STORAGE_ACCESS_KEY")):
                kwargs["account_key"] = v
        elif kind == "fs":
            kwargs["root"] = _env("STORAGE_ROOT")

        op = opendal.AsyncOperator(kind, **kwargs)
        instance = cls(op=op)
        await instance.probe()
        return instance

    async def probe(self) -> None:
        """LIST limit 1. Raises if storage is unreachable."""
        try:
            lister = await self.op.list("/", limit=1)
            async for _ in lister:
                break
        except Exception as e:
            raise RuntimeError(f"storage probe failed: {{e}}") from e
        logger.info("storage probe ok system=%s", "{kind_str}")

    def operator(self) -> "object":
        """Return the underlying opendal AsyncOperator."""
        return self.op
"#,
    )
}

/// Emit `server/src/bin/<job>.rs` per job and append `[[bin]]` entries
/// to `server/Cargo.toml`. Rust-only (the CLI bails earlier if any
/// non-Rust language is paired with `--with-job`).
fn emit_jobs(dest: &Path, vars: &Vars, jobs: &[String]) -> Result<()> {
    let bin_dir = dest.join("server/src/bin");
    std::fs::create_dir_all(&bin_dir).with_context(|| format!("creating {}", bin_dir.display()))?;

    for job in jobs {
        let body = render_job_bin(&vars.service_name, job);
        std::fs::write(bin_dir.join(format!("{job}.rs")), body)
            .with_context(|| format!("writing job binary {job}.rs"))?;
    }

    // Append `[[bin]]` entries to server/Cargo.toml.
    let cargo_path = dest.join("server/Cargo.toml");
    let mut cargo = std::fs::read_to_string(&cargo_path)
        .with_context(|| format!("reading {}", cargo_path.display()))?;
    if !cargo.ends_with('\n') {
        cargo.push('\n');
    }
    cargo.push_str("\n# Background-job binaries. Each one runs to completion (queue\n");
    cargo.push_str("# consumer, scheduled task) rather than serving gRPC.\n");
    for job in jobs {
        cargo.push_str(&format!(
            "[[bin]]\nname = \"{}-{job}\"\npath = \"src/bin/{job}.rs\"\n\n",
            vars.service_name
        ));
    }
    std::fs::write(&cargo_path, cargo)
        .with_context(|| format!("writing {}", cargo_path.display()))?;

    eprintln!(
        "✓ emitted {} background-job binar{}",
        jobs.len(),
        if jobs.len() == 1 { "y" } else { "ies" }
    );
    Ok(())
}

/// Rust source for a single job binary. Kept inline (rather than a
/// .tmpl) because it needs a per-job `{{ job_name }}` substitution
/// that the normal scaffold walker doesn't carry.
fn render_job_bin(service_name: &str, job_name: &str) -> String {
    format!(
        r#"//! `{service_name}-{job_name}` — background-job binary.
//!
//! Bootstraps the same way every micro job does: OTel init, service-
//! identity AuthCtx via `tonin::auth::service_token()`, and pre-wired
//! State (Postgres + Redis from env). No gRPC server is started.
//!
//! Run locally:
//!     cargo run --bin {service_name}-{job_name}
//!
//! In production this becomes a Kubernetes Job (or CronJob) that points
//! at the same image as the main server but overrides the entrypoint to
//! `/usr/local/bin/{service_name}-{job_name}`.

use tonin::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {{
    let ctx = tonin::job::bootstrap("{service_name}-{job_name}").await?;

    tracing::info!(
        target: "{service_name}::{job_name}",
        subject = %ctx.auth.subject,
        has_pg = ctx.state.has_pg(),
        has_redis = ctx.state.has_redis(),
        "job starting",
    );

    // -------------------------------------------------------------
    // Replace this block with your actual job logic.
    //
    // - For queue consumers: loop on a fetch-and-process pattern.
    // - For scheduled work: do the work and return.
    // - For outbound calls to other services, propagate `ctx.auth`:
    //
    //     let mut req = tonic::Request::new(SomeRequest {{ ... }});
    //     ctx.auth.propagate(&mut req);
    //     downstream_client.some_rpc(req).await?;
    // -------------------------------------------------------------

    tracing::info!(target: "{service_name}::{job_name}", "job done");
    Ok(())
}}
"#,
    )
}

/// Emit `server/src/<svc>_server/jobs/<job>.py` per job, plus a `jobs/__init__.py`
/// if needed, plus `[project.scripts]` entries in server/pyproject.toml.
/// Python equivalent of [`emit_jobs`].
fn emit_jobs_python(dest: &Path, vars: &Vars, jobs: &[String]) -> Result<()> {
    let snake = &vars.service_name_snake;
    let pkg_dir = dest
        .join("server/src")
        .join(format!("{snake}_server"))
        .join("jobs");
    std::fs::create_dir_all(&pkg_dir).with_context(|| format!("creating {}", pkg_dir.display()))?;

    // jobs/__init__.py marker
    let init_path = pkg_dir.join("__init__.py");
    if !init_path.exists() {
        std::fs::write(
            &init_path,
            "\"\"\"Background jobs for this service. Each module is runnable via `python -m`.\"\"\"\n",
        )?;
    }

    for job in jobs {
        let body = render_job_py(&vars.service_name, snake, job);
        std::fs::write(pkg_dir.join(format!("{job}.py")), body)
            .with_context(|| format!("writing job {job}.py"))?;
    }

    // Append `[project.scripts]` entries to server/pyproject.toml. The
    // pyproject already has one entry (the main server). We append
    // one console-script per job so `<svc>-<job>` becomes a uv-runnable
    // command after `uv sync`.
    let pyproject_path = dest.join("server/pyproject.toml");
    let mut content = std::fs::read_to_string(&pyproject_path)
        .with_context(|| format!("reading {}", pyproject_path.display()))?;
    let script_lines: String = jobs
        .iter()
        .map(|job| {
            format!(
                "\"{svc}-{job}\" = \"{snake}_server.jobs.{job}:run\"\n",
                svc = vars.service_name,
            )
        })
        .collect();

    // Insert into the existing [project.scripts] block. The scaffold
    // emits exactly one entry (`<svc> = "<svc>_server.main:run"`); we
    // append our lines right after it.
    if let Some(idx) = content.find("[project.scripts]") {
        // Find the next blank-line or section header after the block.
        let block_start = idx + "[project.scripts]".len();
        let rest = &content[block_start..];
        // Find next section (line beginning with `[`) or EOF.
        let mut insertion = block_start;
        for (i, line) in rest.split_inclusive('\n').enumerate() {
            insertion += line.len();
            if i == 0 {
                continue; // skip the header line itself
            }
            if line.starts_with('[') {
                // section header — back up by the line length to
                // insert *before* it.
                insertion -= line.len();
                break;
            }
        }
        content.insert_str(insertion, &script_lines);
    } else {
        // No [project.scripts] yet — append a fresh block at the end.
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("\n[project.scripts]\n");
        content.push_str(&script_lines);
    }
    std::fs::write(&pyproject_path, content)?;

    eprintln!(
        "✓ emitted {} Python background-job module{}",
        jobs.len(),
        if jobs.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

/// Python source for a single job binary. Same shape as `render_job_bin`
/// but async-by-default and built on tonin.job.bootstrap.
fn render_job_py(service_name: &str, service_snake: &str, job_name: &str) -> String {
    format!(
        r#""""`{service_name}-{job_name}` — background-job entry point.

Bootstraps the same way every micro Python job does:
  1. OTel telemetry init via `tonin.job.bootstrap`
  2. Service-identity AuthCtx via `tonin.auth.service_token` (HTTP mint)
  3. State (asyncpg + redis.asyncio, lazily from env)

Async-by-default — there is no sync code path. asyncio.run owns the
event loop; everything inside is `await`-ed.

Run locally::

    uv run {service_name}-{job_name}

In production this becomes a Kubernetes Job (or CronJob) referencing
the same container image but with an overridden entrypoint:
``["python", "-m", "{service_snake}_server.jobs.{job_name}"]``.
"""

from __future__ import annotations

import asyncio
import logging

import tonin

logger = logging.getLogger(__name__)


async def main() -> None:
    ctx = await tonin.job.bootstrap("{service_name}-{job_name}")

    logger.info(
        "job starting subject=%s has_pg=%s has_redis=%s",
        ctx.auth.subject,
        ctx.state.has_pg(),
        ctx.state.has_redis(),
    )

    # ----------------------------------------------------------------
    # Replace this block with your actual job logic.
    #
    # - For queue consumers: loop on a fetch-and-process pattern.
    # - For scheduled work: do the work and return.
    # - For outbound calls, propagate ctx.auth into request metadata:
    #
    #     metadata: list[tuple[str, str]] = []
    #     ctx.auth.propagate(metadata)
    #     reply = await stub.SomeRpc(SomeRequest(...), metadata=metadata)
    # ----------------------------------------------------------------

    logger.info("job done")
    await ctx.state.close()


def run() -> None:
    """Sync entry point so pyproject.toml's `[project.scripts]` can wire it."""
    logging.basicConfig(level=logging.INFO)
    asyncio.run(main())


if __name__ == "__main__":
    run()
"#,
    )
}

fn emit_claude_md(
    dest: &Path,
    name: &str,
    lang: Lang,
    st: ServiceType,
    wm: Option<WebMode>,
) -> Result<()> {
    let lang_label = match lang {
        Lang::Rust => "Rust",
        Lang::Python => "Python (uv-managed)",
        Lang::Ts => match wm {
            Some(WebMode::Bff) => "TypeScript / Next.js BFF",
            _ => "TypeScript / Vite SPA",
        },
    };
    let st_label = st.as_str();
    let body = format!(
        "# {name}

Service scaffolded by `tonin service new`.

> **Coding agents: read `AGENTS.md` first**, then `docs/README.md`.
> This file is just the quick reference card.

## Quick facts for coding agents

- **Language:** {lang}
- **Type:** {kind}
- **Framework:** `tonin` (the `tonin` framework)
- **Observability:** OTLP traces wired via `Service::new`
- **Config:** see `tonin.toml` for declared deps (database, cache, mesh, etc.)
- **Manifests:** k8s YAML is generated; do not hand-edit. Re-run `tonin k8s generate`.

## How to develop

```sh
# Generate / regenerate k8s manifests after editing tonin.toml
tonin k8s generate

# Validate against a real cluster (--dry-run=server)
tonin k8s validate

# Apply
tonin k8s apply
```

## Living documentation

This service uses the `docs/` convention. Before starting any feature:

- `AGENTS.md` — what an agent must do at start + finish
- `docs/README.md` — full convention
- `docs/roadmap.md` — what's done, active, planned
- `docs/capabilities/` — current state, one .md per capability
- `docs/plans/<feature>/` — `PRD.md` + `TechSpec.md` for in-flight work
- `docs/plans/archive/` — completed plans

## Anti-patterns

- Don't import implementation crates directly. Use the `tonin`
  prelude and let `Service::new` install telemetry + propagation.
- Don't hand-write k8s YAML — re-run `tonin k8s generate`.
- Don't commit `db-secret.yaml` with real values. Use `kubectl create
  secret` or an `ExternalSecret` resource instead.
- Don't write code without a PRD + TechSpec in `docs/plans/<feature>/`.
",
        name = name,
        lang = lang_label,
        kind = st_label,
    );
    std::fs::write(dest.join("CLAUDE.md"), body)?;
    Ok(())
}

fn emit_gitignore(dest: &Path) -> Result<()> {
    std::fs::write(
        dest.join(".gitignore"),
        crate::codegen::default_rust_gitignore(),
    )?;
    Ok(())
}

fn emit_docs_tree(dest: &Path, name: &str) -> Result<()> {
    for (rel, contents) in crate::codegen::default_docs_tree(name) {
        let path = dest.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name cannot be empty");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("name cannot contain path separators");
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok || !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        bail!("name must be kebab-case starting with a lowercase letter (got '{name}')");
    }
    Ok(())
}

fn scaffold(
    dest: &Path,
    lang: Lang,
    st: ServiceType,
    wm: Option<WebMode>,
    vars: &Vars,
) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;

    let lang_dir_path = match (lang, st, wm) {
        (Lang::Ts, ServiceType::Web, Some(WebMode::Spa)) => "ts/web-spa".to_string(),
        (Lang::Ts, ServiceType::Web, Some(WebMode::Bff)) => "ts/web-bff".to_string(),
        (Lang::Ts, ServiceType::Web, None) => "ts/web-spa".to_string(), // safety; shouldn't happen
        (Lang::Ts, ServiceType::Backend, _) => "ts/backend".to_string(),
        _ => lang.as_str().to_string(),
    };
    let lang_dir = TEMPLATES
        .get_dir(&lang_dir_path)
        .ok_or_else(|| anyhow!("no templates at {lang_dir_path}"))?;
    let shared_dir = TEMPLATES
        .get_dir("_shared")
        .ok_or_else(|| anyhow!("missing _shared templates"))?;

    let proto_src = read_shared_file(shared_dir, "proto.tmpl")?;
    // Proto file is named after `service_name_snake` so it doubles as a
    // valid Python module name (kebab-case isn't legal in Python imports).
    let proto_out = dest
        .join("proto")
        .join(format!("{}.proto", vars.service_name_snake));
    write_file(&proto_out, &vars.apply(&proto_src))?;

    walk_and_render(lang_dir, lang_dir, dest, vars)?;

    // Ensure migrations/ exists so the Dockerfile's `COPY migrations` works
    // for every scaffolded service (Rust/Python only; web services don't
    // run migrations). A placeholder .gitkeep keeps the empty dir in git.
    if !matches!(st, ServiceType::Web) {
        let migrations_dir = dest.join("migrations");
        std::fs::create_dir_all(&migrations_dir)
            .with_context(|| format!("creating {}", migrations_dir.display()))?;
        write_file(
            &migrations_dir.join(".gitkeep"),
            "# Place sqlx/refinery migration files here.\n",
        )?;
    }

    Ok(())
}

fn walk_and_render(root: &Dir<'_>, cur: &Dir<'_>, dest: &Path, vars: &Vars) -> Result<()> {
    for f in cur.files() {
        let fname = f.path().file_name().unwrap().to_string_lossy();
        if !fname.ends_with(".tmpl") {
            continue;
        }
        let rel = f.path().strip_prefix(root.path()).unwrap();
        let mut out_rel = rel.to_path_buf();
        let stem = out_rel
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches(".tmpl")
            .to_string();
        out_rel.set_file_name(stem);
        // Substitute template vars in path components too. Lets the
        // Python scaffold use directory names like
        //   server/src/{{ service_name_snake }}_server/
        // and have them resolve to the actual package name. The same
        // Vars::apply that processes file content also processes the
        // path, so any var the user puts in either place works.
        let rendered_path_str = vars.apply(&out_rel.to_string_lossy());
        let out_path = dest.join(PathBuf::from(rendered_path_str));
        let src = std::str::from_utf8(f.contents()).expect("template is utf8");
        write_file(&out_path, &vars.apply(src))?;
    }
    for d in cur.dirs() {
        walk_and_render(root, d, dest, vars)?;
    }
    Ok(())
}

fn read_shared_file(dir: &Dir<'_>, name: &str) -> Result<String> {
    let path = format!("{}/{}", dir.path().display(), name);
    let f = dir
        .get_file(&path)
        .ok_or_else(|| anyhow!("missing template {path}"))?;
    Ok(std::str::from_utf8(f.contents())?.to_string())
}

fn write_file(out: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(out, contents).with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

/// `tonin.toml` differs slightly per service shape (web disables MCP,
/// adds `expose = "ingress"`, and records web_mode). Written in code instead
/// of a template so we don't need conditional template logic for one file.
fn write_service_toml(
    dest: &Path,
    vars: &Vars,
    st: ServiceType,
    wm: Option<WebMode>,
) -> Result<()> {
    let (mcp_sidecar, expose_line) = match st {
        ServiceType::Web => (false, "expose      = \"ingress\"\n"),
        ServiceType::Backend => (true, ""),
    };
    let web_mode_line = wm
        .map(|m| format!("web_mode = \"{}\"\n", m.as_str()))
        .unwrap_or_default();
    let body = format!(
        "# tonin.toml — single source of truth for this service.
# `schema` declares the TOML format version this file is written against.
# v1 is backward-compatible: a CLI that knows v2 (when it ships) will still
# read v1 files without changes. Run `tonin service migrate` (when shipped)
# to opt in to a newer schema. Removing this field is fine — the CLI
# defaults to the current schema when it's missing.
schema = \"v1\"

[service]
name     = \"{name}\"
version  = \"0.1.0\"
language = \"{lang}\"
type     = \"{stype}\"
{web_mode}codec    = \"prost\"  # tonic-build is what runs today; buffa codegen plugin is planned

[deploy]
replicas    = 1
mesh        = \"cilium\"   # cilium | istio | linkerd | none
mcp_sidecar = {mcp}
namespace   = \"default\"
{expose}
[resources]
cpu    = \"100m\"
memory = \"128Mi\"

[autoscale]
max_replicas = 3

# Add callees here. Format: <service_name> = \"<namespace>\".
# Network policies are auto-derived from this graph (and its inverse).
[depends_on]
",
        name = vars.service_name,
        lang = vars.language,
        stype = vars.service_type,
        web_mode = web_mode_line,
        mcp = mcp_sidecar,
        expose = expose_line,
    );
    write_file(&dest.join("tonin.toml"), &body)
}

/// Emit an additional client SDK as a sibling folder beside the server's
/// own client. Reuses the per-lang client templates that already power
/// the matching `--lang` scaffolds — so a Rust server can ship a
/// Python client SDK using the exact same template that
/// `--lang python` would generate (just the client side).
fn emit_extra_client(dest: &Path, vars: &Vars, client: ClientLang) -> Result<()> {
    let (template_path, out_subdir) = match client {
        ClientLang::Rust => ("rust/client-rust", "client-rust"),
        ClientLang::Python => ("python/client-python", "client-python"),
        ClientLang::Ts => ("ts/client-ts", "client-ts"),
    };
    let dir = TEMPLATES.get_dir(template_path).ok_or_else(|| {
        anyhow!("no template directory for client-lang {client:?} at {template_path}")
    })?;

    // walk_and_render computes output paths relative to the template
    // root. Since we want output under `<dest>/<out_subdir>/...`, we
    // pass a dest path that already includes the subdir.
    let out_root = dest.join(out_subdir);
    walk_and_render(dir, dir, &out_root, vars)?;

    eprintln!("✓ emitted {} client SDK at {out_subdir}/", client.as_str());
    Ok(())
}

fn pregenerate_k8s(dest: &Path) -> Result<()> {
    let toml = dest.join("tonin.toml");
    let plan = Plan::load(&toml).context("loading freshly-scaffolded tonin.toml")?;
    let files = render::render(&plan).context("rendering k8s manifests")?;
    let k8s_dir = dest.join("k8s");
    std::fs::create_dir_all(&k8s_dir)?;
    for f in &files {
        std::fs::write(k8s_dir.join(&f.path), &f.contents)?;
    }
    eprintln!(
        "✓ pre-generated {} k8s files in {}/k8s/",
        files.len(),
        dest.display()
    );
    Ok(())
}

fn maybe_add_to_workspace(dest: &Path) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(ws_root) = find_workspace_root(&cwd) else {
        return Ok(());
    };
    let ws_toml = ws_root.join("Cargo.toml");

    // The scaffolded service is a 3-folder layout: `server/` and
    // `client-rust/` are each independent Cargo crates. To integrate
    // with a parent workspace we add BOTH as members.
    let dest_abs = dest.canonicalize().unwrap_or_else(|_| cwd.join(dest));
    let rel = dest_abs
        .strip_prefix(&ws_root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| dest.display().to_string());
    let server_member = format!("{rel}/server");
    let client_member = format!("{rel}/client-rust");

    let prompt = format!(
        "Detected Cargo workspace at {}.\n  Add '{server_member}' and '{client_member}' as members? [Y/n] ",
        ws_root.display(),
    );
    if !dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        eprintln!("skipped workspace integration");
        return Ok(());
    }

    let text = std::fs::read_to_string(&ws_toml)?;
    let mut doc: toml_edit::DocumentMut = text.parse().context("parsing workspace Cargo.toml")?;
    let members = doc
        .get_mut("workspace")
        .and_then(|w| w.as_table_mut())
        .and_then(|t| t.get_mut("members"))
        .and_then(|m| m.as_array_mut())
        .ok_or_else(|| anyhow!("[workspace] members not found in {}", ws_toml.display()))?;
    for m in [&server_member, &client_member] {
        if !members.iter().any(|v| v.as_str() == Some(m.as_str())) {
            members.push(m.clone());
        }
    }
    std::fs::write(&ws_toml, doc.to_string())?;
    eprintln!(
        "✓ added '{server_member}' and '{client_member}' to workspace members in {}",
        ws_toml.display()
    );
    Ok(())
}

fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start.to_path_buf());
    while let Some(p) = cur {
        let candidate = p.join("Cargo.toml");
        if candidate.exists()
            && let Ok(text) = std::fs::read_to_string(&candidate)
            && let Ok(doc) = text.parse::<toml_edit::DocumentMut>()
            && doc.get("workspace").is_some()
        {
            return Some(p);
        }
        cur = p.parent().map(Path::to_path_buf);
    }
    None
}

fn print_next_steps(
    name: &str,
    lang: Lang,
    st: ServiceType,
    wm: Option<WebMode>,
    jobs: &[String],
    storage: Option<StorageKind>,
    extra_clients: &[ClientLang],
) {
    let wm_label = wm.map(|m| format!(",{}", m.as_str())).unwrap_or_default();
    eprintln!();
    eprintln!(
        "✓ created service '{name}' (lang={}, type={}{wm_label})",
        lang.as_str(),
        st.as_str()
    );
    eprintln!();
    eprintln!("next steps:");
    eprintln!("  cd {name}");
    match (lang, st, wm) {
        (Lang::Rust, _, _) => {
            eprintln!("  cargo build                  # compile the stub server");
            eprintln!("  tonin k8s validate        # check generated YAML against your cluster");
        }
        (Lang::Python, _, _) => {
            eprintln!("  (cd client-python && bash codegen.sh)   # generate client stubs");
            eprintln!("  (cd server        && bash codegen.sh)   # generate server stubs");
            eprintln!("  cd server && uv sync                    # creates .venv");
            eprintln!("  uv run pytest                           # e2e tests");
            eprintln!("  uv run {name}                            # start the server on :50051");
        }
        (Lang::Ts, ServiceType::Web, Some(WebMode::Spa)) => {
            eprintln!("  npm install");
            eprintln!("  npm run gen                  # codegen from proto/");
            eprintln!("  npm run dev                  # Vite dev server on :5173");
        }
        (Lang::Ts, ServiceType::Web, Some(WebMode::Bff)) => {
            eprintln!("  npm install");
            eprintln!("  npm run gen                  # codegen from proto/");
            eprintln!("  npm run dev                  # Next.js dev server on :3000");
        }
        (Lang::Ts, ServiceType::Backend, _) | (Lang::Ts, _, None) => {
            eprintln!("  npm install");
            eprintln!("  npm run gen");
            eprintln!("  npm run dev                  # tsx watch on :50051");
        }
    }
    eprintln!("  tonin k8s apply            # deploy to current kubectl context");
    if !jobs.is_empty() {
        eprintln!();
        eprintln!("background jobs:");
        for job in jobs {
            match lang {
                Lang::Rust => eprintln!("  cargo run --bin {name}-{job}"),
                Lang::Python => eprintln!("  uv run {name}-{job}"),
                Lang::Ts => {}
            }
        }
    }
    if let Some(kind) = storage {
        eprintln!();
        eprintln!("storage ({}):", kind.as_str());
        match kind {
            StorageKind::S3 => {
                eprintln!("  export STORAGE_BUCKET=...                # required");
                eprintln!("  export STORAGE_REGION=us-west-2");
                eprintln!("  export STORAGE_ENDPOINT=http://localhost:9000   # MinIO etc.");
                eprintln!("  export STORAGE_ACCESS_KEY=... STORAGE_SECRET_KEY=...");
            }
            StorageKind::Gcs => {
                eprintln!("  export STORAGE_BUCKET=...                # required");
                eprintln!("  export STORAGE_CREDENTIAL_PATH=/path/to/key.json");
            }
            StorageKind::Azure => {
                eprintln!("  export STORAGE_CONTAINER=...             # required");
                eprintln!("  export STORAGE_ACCOUNT=... STORAGE_ACCESS_KEY=...");
            }
            StorageKind::Local => {
                eprintln!("  export STORAGE_ROOT=./.data              # required");
            }
        }
        eprintln!("  # Boot probe: LIST limit 1. Misconfig → service refuses to start.");
    }
    if !extra_clients.is_empty() {
        eprintln!();
        eprintln!("extra client SDKs:");
        for cl in extra_clients {
            match cl {
                ClientLang::Python => {
                    eprintln!("  (cd client-python && bash codegen.sh)   # regenerate stubs");
                }
                ClientLang::Ts => {
                    eprintln!("  (cd client-ts && npm install && npm run gen)");
                }
                ClientLang::Rust => {
                    eprintln!("  (cd client-rust && cargo build)");
                }
            }
        }
    }
}
