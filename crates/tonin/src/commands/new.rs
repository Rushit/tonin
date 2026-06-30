//! `tonin service new <name> --lang <rust|python|ts> [--type web|backend]`.
//!
//! Scaffold templates use plain `{{ var }}` substitution — no loops, no
//! conditionals. Done with `str::replace` (not a template engine) so the
//! syntax can't collide with TSX/JS object literals or anything else inside
//! the templates. The k8s renderer is a separate pipeline that does use
//! Tera, because cross-service network policies need real loops.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use convert_case::{Case, Casing};
use flate2::read::GzDecoder;
use include_dir::{Dir, include_dir};
use tempfile::TempDir;

use super::service::{ClientLang, Lang, ServiceType, StorageKind, WebMode};

static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates/service");

/// Which set of templates to use for this scaffolding invocation.
enum TemplateSource {
    /// Use the templates baked into the binary at compile time (default).
    Embedded,
    /// Use a template repo downloaded from GitHub. `_tmpdir` keeps the
    /// extracted archive on disk until the end of `run()`; `variant_root`
    /// points at `<tmpdir>/<archive-top-level>/variants/<variant>/`.
    Fetched {
        _tmpdir: TempDir,
        variant_root: PathBuf,
    },
}

/// Parse `--template-repo` into `(owner/repo, git_ref, is_tag)`.
///
/// Accepts:
///   "github.com/Org/repo"          → ("Org/repo", "main", false)
///   "Org/repo"                     → ("Org/repo", "main", false)
///   "github.com/Org/repo@v1.2.3"   → ("Org/repo", "v1.2.3", true)
///   "Org/repo@some-branch"         → ("Org/repo", "some-branch", false)
fn parse_repo_ref(raw: &str) -> (String, String, bool) {
    let stripped = raw.trim_start_matches("github.com/");
    let (repo, git_ref) = stripped.split_once('@').unwrap_or((stripped, "main"));
    let is_tag = git_ref
        .strip_prefix('v')
        .map(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(false);
    (repo.to_string(), git_ref.to_string(), is_tag)
}

/// Download the tarball for `repo_str` from GitHub, extract to a `TempDir`,
/// locate `variants/<variant>/` inside, and return a `TemplateSource::Fetched`.
fn fetch_template_repo(repo_str: &str, variant: &str) -> Result<TemplateSource> {
    let (repo, git_ref, is_tag) = parse_repo_ref(repo_str);
    let url = if is_tag {
        format!("https://github.com/{repo}/archive/refs/tags/{git_ref}.tar.gz")
    } else {
        format!("https://github.com/{repo}/archive/refs/heads/{git_ref}.tar.gz")
    };

    eprintln!("fetching template repo: {url}");

    let rt = tokio::runtime::Runtime::new().context("creating tokio runtime for template fetch")?;
    let bytes: Vec<u8> = rt
        .block_on(async {
            reqwest::Client::builder()
                .user_agent(concat!("tonin-cli/", env!("CARGO_PKG_VERSION")))
                .build()?
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await
        })
        .map_err(|e| anyhow!("downloading template repo {url}: {e}"))?
        .to_vec();

    let tmpdir = TempDir::new().context("creating tempdir for template repo")?;
    let gz = GzDecoder::new(std::io::Cursor::new(&bytes));
    let mut archive = tar::Archive::new(gz);
    archive.set_overwrite(true);
    archive
        .unpack(tmpdir.path())
        .context("extracting template repo tarball")?;

    // GitHub tarballs have a single top-level directory named `<repo>-<ref>/`.
    let top_level = std::fs::read_dir(tmpdir.path())
        .context("reading extracted tempdir")?
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .ok_or_else(|| anyhow!("template repo tarball is empty or malformed"))?;

    // Warn if the repo declares a minimum CLI version we don't meet.
    let version_toml = top_level.join("version.toml");
    if version_toml.exists() {
        check_version_compat(&version_toml);
    }

    let variant_root = top_level.join("variants").join(variant);
    if !variant_root.exists() {
        let available = list_dir_names(&top_level.join("variants"));
        bail!(
            "template repo has no 'variants/{variant}/' directory (available: {available}). \
             Try a different --template-repo or omit --flat."
        );
    }

    Ok(TemplateSource::Fetched {
        _tmpdir: tmpdir,
        variant_root,
    })
}

/// Parse `version.toml` from the fetched repo and print a warning if
/// `cli_min_version` exceeds the running binary. Non-fatal.
fn check_version_compat(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return;
    };
    if let Some(min) = doc.get("cli_min_version").and_then(|v| v.as_str()) {
        let current = env!("CARGO_PKG_VERSION");
        if super::plugin::version_lt(current, min) {
            eprintln!(
                "warning: template repo requires tonin >= {min} (you have {current}). \
                 Run `tonin upgrade` if scaffolding fails."
            );
        }
    }
}

/// Comma-separated list of sub-directory names inside `dir` for error messages.
fn list_dir_names(dir: &Path) -> String {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Filesystem analog of `walk_and_render` — renders `.tmpl` files from `cur`
/// (rooted at `root`) into `dest`, substituting template vars in both content
/// and path components.
fn walk_and_render_fs(root: &Path, cur: &Path, dest: &Path, vars: &Vars) -> Result<()> {
    for entry in std::fs::read_dir(cur).with_context(|| format!("reading {}", cur.display()))? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_and_render_fs(root, &path, dest, vars)?;
        } else if ft.is_file() {
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy();
            if !fname_str.ends_with(".tmpl") {
                continue;
            }
            let rel = path.strip_prefix(root).with_context(|| {
                format!("strip_prefix {} from {}", root.display(), path.display())
            })?;
            let mut out_rel = rel.to_path_buf();
            let stem = out_rel
                .file_name()
                .unwrap()
                .to_string_lossy()
                .trim_end_matches(".tmpl")
                .to_string();
            out_rel.set_file_name(stem);
            let rendered_path = vars.apply(&out_rel.to_string_lossy());
            let out_path = dest.join(PathBuf::from(rendered_path));
            let src = std::fs::read_to_string(&path)
                .with_context(|| format!("reading template {}", path.display()))?;
            write_file(&out_path, &vars.apply(&src))?;
        }
    }
    Ok(())
}

/// Filesystem analog of `read_shared_file` — reads a named file from `shared_dir`.
fn read_shared_file_fs(shared_dir: &Path, name: &str) -> Result<String> {
    let path = shared_dir.join(name);
    std::fs::read_to_string(&path)
        .with_context(|| format!("reading shared template {}", path.display()))
}

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
    template_repo: Option<&str>,
    flat: bool,
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

    // Resolve the template source once. `TemplateSource::Fetched` holds the
    // `TempDir` alive for the entire duration of `run()`.
    let source = match template_repo {
        None => TemplateSource::Embedded,
        Some(repo) => {
            let variant = if flat { "flat" } else { "default" };
            fetch_template_repo(repo, variant)?
        }
    };

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

    // Default: three-crate workspace layout (proto + server + rs).
    // Use --flat to get the old single-directory structure instead.
    if matches!(lang, Lang::Rust) && !flat {
        if dest.exists() {
            bail!(
                "{} already exists; pick another name or delete it first",
                dest.display()
            );
        }
        std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
        emit_workspace_layout(&dest, &vars, extra_clients, &source)?;
        print_workspace_next_steps(name, extra_clients);
        return Ok(());
    }

    scaffold(&dest, lang, st, wm, &vars, &source)?;
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
        emit_extra_client(&dest, &vars, *extra, &source)?;
    }
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
- **Framework:** `tonin`
- **Observability:** OTLP traces wired via `Service::new`
- **Config:** `tonin.toml` — single source of truth for deps, mesh, replicas, resources

## Scaffolding a new sibling service

Use `tonin service new` with `--template-repo` to pull from the canonical
template repository instead of the CLI's built-in copy:

```sh
# Scaffold with the standard tonin templates
tonin service new <name> --lang rust --template-repo github.com/Rushit/tonin-templates

# Scaffold with your org's production templates (distroless, CI workflows, migration checks)
tonin service new <name> --lang rust --template-repo github.com/your-org/your-templates

# Pin to a specific release
tonin service new <name> --lang rust --template-repo github.com/Rushit/tonin-templates@v0.4.0

# Without --template-repo the CLI uses its built-in embedded templates
tonin service new <name> --lang python
```

The flag accepts `github.com/Org/repo` or just `Org/repo`. Append `@ref` for a branch
or tag. The CLI downloads the tarball, checks `version.toml` compatibility, and renders
`variants/default/<lang>/` (or `variants/flat/<lang>/` with `--flat`).

## How to develop

```sh
# Regenerate Helm charts after editing tonin.toml
tonin helm generate

# Preview against a real cluster
tonin helm diff --env prod

# Deploy
tonin helm upgrade --env prod
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
- Don't hand-write Helm chart files — re-run `tonin helm generate`.
- Don't commit secrets with real values. Use `kubectl create secret`
  or an `ExternalSecret` resource instead.
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
    std::fs::write(dest.join(".gitignore"), default_rust_gitignore())?;
    Ok(())
}

fn emit_docs_tree(dest: &Path, name: &str) -> Result<()> {
    for (rel, contents) in default_docs_tree(name) {
        let path = dest.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

fn default_docs_tree(service_name: &str) -> Vec<(String, String)> {
    vec![
        ("AGENTS.md".to_string(), docs_agents_md(service_name)),
        ("docs/README.md".to_string(), docs_readme_md(service_name)),
        ("docs/roadmap.md".to_string(), docs_roadmap_md(service_name)),
        ("docs/capabilities/.gitkeep".to_string(), "# Add one .md per capability the service offers.\n# See ../README.md for the convention.\n".to_string()),
        ("docs/plans/.gitkeep".to_string(), "# Add one folder per active feature/project.\n# Each folder must contain PRD.md and TechSpec.md.\n# See ../README.md for the convention.\n".to_string()),
        ("docs/plans/archive/.gitkeep".to_string(), "# Completed plans move here on feature close.\n# See ../../README.md for the convention.\n".to_string()),
    ]
}

fn docs_agents_md(service_name: &str) -> String {
    format!(
        "# AGENTS.md — {name}

This file is the entry point for any coding agent working in this repo.

## Before you write code

1. Read `docs/README.md` for the docs convention.
2. Read `CLAUDE.md` for quick facts about this service (language, framework, config).
3. Read `docs/roadmap.md` to understand what's done, active, and planned.
4. Read `docs/capabilities/*.md` for what this service already does (no need to rediscover).

## Scaffolding a new service

Use `tonin service new` with `--template-repo` to pull from a remote template repo:

```sh
# Standard tonin templates
tonin service new <name> --lang rust --template-repo github.com/Rushit/tonin-templates

# Production templates (distroless, CI, migration safety, CLAUDE.md)
tonin service new <name> --lang rust --template-repo github.com/your-org/your-templates

# Pin to a tag
tonin service new <name> --lang rust --template-repo github.com/Rushit/tonin-templates@v0.4.0

# Built-in embedded templates (no network required)
tonin service new <name> --lang python
```

See `CLAUDE.md` for the full flag reference.

## When starting a new feature / project

Create `docs/plans/<feature-name>/` and seed it with two files:

- **`PRD.md`** — Product Requirements Document.
- **`TechSpec.md`** — Technical Specification.

Both files exist before any code change.

## When finishing a feature

1. Move the plan to archive: `mv docs/plans/<feature> docs/plans/archive/<feature>`.
2. Update `docs/capabilities/` with a new or updated `.md` for the capability.
3. Update `docs/roadmap.md` — move the entry from \"active\" to \"done\".
",
        name = service_name,
    )
}

fn docs_readme_md(service_name: &str) -> String {
    format!(
        "# docs/

Living documentation for `{name}`.

## Layout

```
docs/
├── README.md              ← you are here
├── roadmap.md             ← rolling list: done / active / future
├── capabilities/          ← one .md per public capability (current state)
└── plans/
    ├── <feature>/         ← in-flight feature work (PRD.md + TechSpec.md)
    └── archive/           ← finished work
```
",
        name = service_name,
    )
}

fn docs_roadmap_md(service_name: &str) -> String {
    format!(
        "# {name} — roadmap

## Done

(empty)

## Active

(empty)

## Future

(empty)
",
        name = service_name,
    )
}

fn default_rust_gitignore() -> &'static str {
    r#"# Rust build artifacts.
/target
**/*.rs.bk

# Python (when scaffolded with --lang python).
__pycache__/
*.py[cod]
.venv/
.python-version

# Node / TS (when scaffolded with --lang ts).
node_modules/
.next/
dist/

# IDE / OS noise.
.DS_Store
.idea/
.vscode/
*.swp
*.swo

# Local secrets — never commit.
*.local.yaml
*.local.toml
.env
.env.*

# Claude Code per-user settings.
.claude/settings.local.json

# NOTE: Cargo.lock and proto/ are INTENTIONALLY committed.
"#
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
    source: &TemplateSource,
) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;

    let lang_dir_path = match (lang, st, wm) {
        (Lang::Ts, ServiceType::Web, Some(WebMode::Spa)) => "ts/web-spa".to_string(),
        (Lang::Ts, ServiceType::Web, Some(WebMode::Bff)) => "ts/web-bff".to_string(),
        (Lang::Ts, ServiceType::Web, None) => "ts/web-spa".to_string(), // safety; shouldn't happen
        (Lang::Ts, ServiceType::Backend, _) => "ts/backend".to_string(),
        _ => lang.as_str().to_string(),
    };

    let proto_src = match source {
        TemplateSource::Embedded => {
            let shared_dir = TEMPLATES
                .get_dir("_shared")
                .ok_or_else(|| anyhow!("missing _shared templates"))?;
            read_shared_file(shared_dir, "proto.tmpl")?
        }
        TemplateSource::Fetched { variant_root, .. } => {
            read_shared_file_fs(&variant_root.join("_shared"), "proto.tmpl")?
        }
    };
    // Proto file is named after `service_name_snake` so it doubles as a
    // valid Python module name (kebab-case isn't legal in Python imports).
    let proto_out = dest
        .join("proto")
        .join(format!("{}.proto", vars.service_name_snake));
    write_file(&proto_out, &vars.apply(&proto_src))?;

    match source {
        TemplateSource::Embedded => {
            let lang_dir = TEMPLATES
                .get_dir(&lang_dir_path)
                .ok_or_else(|| anyhow!("no templates at {lang_dir_path}"))?;
            walk_and_render(lang_dir, lang_dir, dest, vars)?;
        }
        TemplateSource::Fetched { variant_root, .. } => {
            let lang_dir = variant_root.join(&lang_dir_path);
            if !lang_dir.exists() {
                bail!("fetched template repo has no directory for '{lang_dir_path}'");
            }
            walk_and_render_fs(&lang_dir, &lang_dir, dest, vars)?;
        }
    }

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
fn emit_extra_client(
    dest: &Path,
    vars: &Vars,
    client: ClientLang,
    source: &TemplateSource,
) -> Result<()> {
    let (template_path, out_subdir) = match client {
        ClientLang::Rust => ("rust/client-rust", "client-rust"),
        ClientLang::Python => ("python/client-python", "client-python"),
        ClientLang::Ts => ("ts/client-ts", "client-ts"),
    };

    // walk_and_render computes output paths relative to the template
    // root. Since we want output under `<dest>/<out_subdir>/...`, we
    // pass a dest path that already includes the subdir.
    let out_root = dest.join(out_subdir);
    match source {
        TemplateSource::Embedded => {
            let dir = TEMPLATES.get_dir(template_path).ok_or_else(|| {
                anyhow!("no template directory for client-lang {client:?} at {template_path}")
            })?;
            walk_and_render(dir, dir, &out_root, vars)?;
        }
        TemplateSource::Fetched { variant_root, .. } => {
            let dir = variant_root.join(template_path);
            if !dir.exists() {
                bail!("fetched template repo has no client template at '{template_path}'");
            }
            walk_and_render_fs(&dir, &dir, &out_root, vars)?;
        }
    }

    eprintln!("✓ emitted {} client SDK at {out_subdir}/", client.as_str());
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
            eprintln!("  tonin helm generate          # render Helm chart from tonin.toml");
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
    eprintln!("  tonin helm upgrade --env prod   # deploy to cluster (install tonin-helm once)");
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
                    eprintln!(
                        "  # depends on `tonin-client`; until it's on npm: npm link tonin-client"
                    );
                }
                ClientLang::Rust => {
                    eprintln!("  (cd client-rust && cargo build)");
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Workspace layout scaffold (--workspace-layout)
// ─────────────────────────────────────────────────────────────────────────

/// Scaffold the workspace layout: <name>-proto, <name>-server, <name>-rs,
/// plus optional language clients (<name>-py, <name>-ts).
fn emit_workspace_layout(
    dest: &Path,
    vars: &Vars,
    extras: &[ClientLang],
    source: &TemplateSource,
) -> Result<()> {
    let name = &vars.service_name;
    let snake = &vars.service_name_snake;
    let camel = &vars.service_camel;
    let proto_mod = &vars.service_proto_module;

    // Proto content from the shared template.
    let proto_content = match source {
        TemplateSource::Embedded => {
            let shared = TEMPLATES
                .get_dir("_shared")
                .ok_or_else(|| anyhow!("missing _shared templates"))?;
            vars.apply(&read_shared_file(shared, "proto.tmpl")?)
        }
        TemplateSource::Fetched { variant_root, .. } => vars.apply(&read_shared_file_fs(
            &variant_root.join("_shared"),
            "proto.tmpl",
        )?),
    };

    // ── workspace root ──────────────────────────────────────────────────
    write_file(&dest.join("Cargo.toml"), &ws_cargo_toml(name))?;
    write_file(&dest.join(".cargo/config.toml"), &ws_cargo_config(name))?;
    write_file(&dest.join("CLAUDE.md"), &ws_claude_md(name, snake, camel))?;
    write_file(&dest.join("AGENTS.md"), &ws_agents_md(name))?;
    write_file(&dest.join(".gitignore"), &ws_gitignore())?;

    // ── <name>-proto ─────────────────────────────────────────────────────
    let proto_dir = dest.join(format!("{name}-proto"));
    write_file(&proto_dir.join("Cargo.toml"), &proto_cargo_toml(name))?;
    write_file(&proto_dir.join("build.rs"), &proto_build_rs(snake))?;
    write_file(
        &proto_dir.join(format!("proto/{snake}.proto")),
        &proto_content,
    )?;
    write_file(&proto_dir.join("src/lib.rs"), &proto_lib_rs(snake))?;
    write_file(
        &proto_dir.join("CLAUDE.md"),
        &sub_claude_md(name, "proto", snake, camel, proto_mod),
    )?;
    write_file(&proto_dir.join("AGENTS.md"), "@CLAUDE.md\n")?;
    write_file(&proto_dir.join(".gitignore"), "/target/\n")?;

    // ── <name>-server ────────────────────────────────────────────────────
    let server_dir = dest.join(format!("{name}-server"));
    write_file(&server_dir.join("Cargo.toml"), &server_cargo_toml(name))?;
    write_file(&server_dir.join("tonin.toml"), &server_tonin_toml(name))?;
    write_file(
        &server_dir.join("src/main.rs"),
        &server_main_rs(name, snake, camel, proto_mod),
    )?;
    write_file(&server_dir.join("src/lib.rs"), &server_lib_rs(camel))?;
    write_file(
        &server_dir.join("src/service.rs"),
        &server_service_rs(snake, camel, proto_mod),
    )?;
    write_file(
        &server_dir.join("migrations/.gitkeep"),
        "# sqlx migration files go here.\n",
    )?;
    write_file(
        &server_dir.join("tests/contract_e2e_test.rs"),
        &server_contract_test(snake, camel, proto_mod),
    )?;
    write_file(
        &server_dir.join("CLAUDE.md"),
        &sub_claude_md(name, "server", snake, camel, proto_mod),
    )?;
    write_file(&server_dir.join("AGENTS.md"), "@CLAUDE.md\n")?;
    write_file(&server_dir.join(".gitignore"), &server_gitignore())?;

    // ── <name>-rs ────────────────────────────────────────────────────────
    let rs_dir = dest.join(format!("{name}-rs"));
    write_file(&rs_dir.join("Cargo.toml"), &rs_cargo_toml(name))?;
    write_file(
        &rs_dir.join("src/lib.rs"),
        &rs_lib_rs(name, snake, camel, proto_mod),
    )?;
    write_file(
        &rs_dir.join("CLAUDE.md"),
        &sub_claude_md(name, "rs", snake, camel, proto_mod),
    )?;
    write_file(&rs_dir.join("AGENTS.md"), "@CLAUDE.md\n")?;
    write_file(&rs_dir.join(".gitignore"), "/target/\n")?;

    // ── Makefiles ────────────────────────────────────────────────────────
    write_file(&dest.join("Makefile"), &ws_makefile(name))?;
    write_file(
        &dest.join(format!("{name}-proto/Makefile")),
        &proto_makefile(name),
    )?;
    write_file(
        &dest.join(format!("{name}-server/Makefile")),
        &server_makefile(name),
    )?;
    write_file(
        &dest.join(format!("{name}-rs/Makefile")),
        &rs_makefile(name),
    )?;

    // ── GitHub Actions CI ─────────────────────────────────────────────
    let gh_dir = dest.join(".github/workflows");
    std::fs::create_dir_all(&gh_dir).with_context(|| format!("creating {}", gh_dir.display()))?;
    write_file(&gh_dir.join("ci.yml"), &ws_github_ci_yml(name))?;

    // ── E2E blackbox test crate ───────────────────────────────────────
    let e2e_dir = dest.join(format!("{name}-e2e"));
    std::fs::create_dir_all(e2e_dir.join("tests/common"))
        .with_context(|| format!("creating {name}-e2e/tests/common/"))?;
    write_file(&e2e_dir.join("Cargo.toml"), &e2e_cargo_toml(name))?;
    write_file(&e2e_dir.join("Makefile"), &e2e_makefile(name))?;
    write_file(&e2e_dir.join("tests/common/mod.rs"), &e2e_common_mod(name))?;
    write_file(&e2e_dir.join("tests/contract.rs"), &e2e_contract_test(name))?;

    // Optional language clients: <name>-py, <name>-ts, etc.
    for &client in extras {
        emit_workspace_client(dest, vars, client, source)?;
    }

    let extra_labels: Vec<&str> = extras
        .iter()
        .map(|c| match c {
            ClientLang::Python => "greeter-py",
            ClientLang::Ts => "greeter-ts",
            ClientLang::Rust => "",
        })
        .filter(|s| !s.is_empty())
        .collect();
    let extra_str = if extra_labels.is_empty() {
        String::new()
    } else {
        format!(
            " / {}",
            extras
                .iter()
                .filter(|&&c| !matches!(c, ClientLang::Rust))
                .map(|c| format!(
                    "{name}-{}",
                    if matches!(c, ClientLang::Python) {
                        "py"
                    } else {
                        "ts"
                    }
                ))
                .collect::<Vec<_>>()
                .join(" / ")
        )
    };
    eprintln!("✓ workspace: {name}-proto / {name}-server / {name}-rs{extra_str}");
    Ok(())
}

/// Render a language client into the workspace as `<name>-py` or `<name>-ts`.
/// Skips Rust (already covered by `<name>-rs`).
fn emit_workspace_client(
    dest: &Path,
    vars: &Vars,
    client: ClientLang,
    source: &TemplateSource,
) -> Result<()> {
    let (template_path, suffix) = match client {
        ClientLang::Python => ("python/client-python", "py"),
        ClientLang::Ts => ("ts/client-ts", "ts"),
        ClientLang::Rust => return Ok(()), // already emitted as <name>-rs
    };

    let name = &vars.service_name;
    let out_dir = dest.join(format!("{name}-{suffix}"));
    match source {
        TemplateSource::Embedded => {
            let dir = TEMPLATES
                .get_dir(template_path)
                .ok_or_else(|| anyhow!("no template at {template_path}"))?;
            walk_and_render(dir, dir, &out_dir, vars)?;
        }
        TemplateSource::Fetched { variant_root, .. } => {
            let dir = variant_root.join(template_path);
            if !dir.exists() {
                bail!("fetched template repo has no client template at '{template_path}'");
            }
            walk_and_render_fs(&dir, &dir, &out_dir, vars)?;
        }
    }

    // Patch the package name from `<name>-client` → `<name>-{suffix}` so
    // the directory name and the published package name align.
    match client {
        ClientLang::Python => patch_file(
            &out_dir.join("pyproject.toml"),
            &format!("{name}-client"),
            &format!("{name}-py"),
        )?,
        ClientLang::Ts => patch_file(
            &out_dir.join("package.json"),
            &format!("\"name\": \"{name}-client\""),
            &format!("\"name\": \"{name}-ts\""),
        )?,
        ClientLang::Rust => {}
    }

    // CLAUDE.md / AGENTS.md / .gitignore
    write_file(
        &out_dir.join("CLAUDE.md"),
        &ws_client_claude_md(name, suffix, vars),
    )?;
    write_file(&out_dir.join("AGENTS.md"), "@CLAUDE.md\n")?;
    write_file(&out_dir.join(".gitignore"), &ws_client_gitignore(client))?;

    eprintln!("✓ emitted {suffix} client → {name}-{suffix}/");
    Ok(())
}

fn patch_file(path: &Path, from: &str, to: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    std::fs::write(path, content.replace(from, to))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ── file content helpers ────────────────────────────────────────────────

fn ws_cargo_toml(name: &str) -> String {
    format!(
        r#"[workspace]
resolver = "2"
members = [
    "{name}-proto",
    "{name}-server",
    "{name}-rs",
    "{name}-e2e",
]

[workspace.package]
version  = "0.1.0"
edition  = "2024"
license  = "LicenseRef-Commercial"

[workspace.dependencies]
{name}-proto = {{ path = "{name}-proto" }}
{name}-rs    = {{ path = "{name}-rs" }}
tonin        = "0.4"
tonin-client = "0.4"
tonic        = "0.12"
prost        = "0.13"
prost-types  = "0.13"
tokio        = {{ version = "1", features = ["full"] }}
async-trait  = "0.1"
tracing      = "0.1"
thiserror    = "2"
tokio-stream = {{ version = "0.1", features = ["net"] }}
anyhow       = "1"
testcontainers         = "0.27"
testcontainers-modules = {{ version = "0.15", features = ["postgres", "redis"] }}
sqlx = {{ version = "0.8", features = ["postgres", "runtime-tokio-native-tls", "macros"] }}
"#
    )
}

fn ws_cargo_config(name: &str) -> String {
    format!(
        r#"[build]
target-dir = "/tmp/{name}-target"
"#
    )
}

fn ws_claude_md(name: &str, snake: &str, camel: &str) -> String {
    format!(
        r#"# {name} — Claude guidance

Cargo workspace scaffolded by `tonin service new`. Read this file first,
then the crate-level CLAUDE.md for the specific code you're touching.

## Crate map

| Crate | Role | Published |
|-------|------|-----------|
| `{name}-proto` | gRPC contract (`{snake}.v1`) — source of truth for the wire format | Yes |
| `{name}-server` | tonin gRPC binary (`{camel}` service) | Binary only |
| `{name}-rs` | Rust client library for callers | Yes |

Extra clients (if scaffolded): `{name}-py` (Python), `{name}-ts` (TypeScript).

## Key commands

```bash
cargo build && cargo test --workspace
cargo clippy --workspace -- -D warnings
cd {name}-server && tonin helm generate  # render Helm chart from tonin.toml
```

## Versioning policy

| Layer | Scheme | Breaking change rule |
|-------|--------|----------------------|
| Proto API | `{snake}.v1` package; bump to `v2` only on wire break | Field renumber, type change, removal |
| DB schema | Sequential `0001_*.sql`; files immutable once merged | Any destructive change |
| Rust crates | Semver — MAJOR tracks proto MAJOR | Any non-backward-compatible API change |
| Python pkg | Semver — same MAJOR as Rust | Same |
| npm pkg | Semver — same MAJOR as Rust | Same |

## Backward compatibility — non-negotiable

### Proto
- **Safe:** add field (new number), add enum value, add RPC, rename Rust binding
- **UNSAFE — never without bumping to v2:**
  - Renumber a field (wire breaking)
  - Change a field's type
  - Remove a field — instead add `reserved 3;` and `reserved "old_name";`

### Database migrations
- **Safe:** `ADD COLUMN` with `DEFAULT`, `CREATE INDEX CONCURRENTLY`, `CREATE TABLE`
- **UNSAFE — use two-step deploy:**
  - `DROP COLUMN` → stop using it in code (deploy), then drop in a later migration
  - `RENAME COLUMN` → add new + backfill + drop old (three separate deploys)
  - `ALTER COLUMN TYPE` → add new column, migrate data, drop old (three deploys)
- Migration files are **immutable once merged** — never edit a committed migration

## Workspace-level rules
- `edition = "2024"`, `license = "LicenseRef-Commercial"` on all crates (inherited via workspace)
- Cargo target dir: `/tmp/{name}-target` (`.cargo/config.toml`) — never commit `/target/`
- Never hand-edit generated Helm chart files — always re-run `tonin helm generate`
- `k8s/secrets.yaml` and `k8s/db-secret.yaml` are auto-gitignored; never commit with real values
"#
    )
}

fn ws_agents_md(name: &str) -> String {
    format!(
        r#"@CLAUDE.md

# {name} — Agent instructions

> Universal instructions for any AI agent (Claude Code, Cursor, Windsurf, Copilot…).
> Full detail in `CLAUDE.md` (auto-loaded above).

## Essential commands

```bash
cargo build && cargo test --workspace
cargo clippy --workspace -- -D warnings
cd {name}-server && tonin helm generate  # render Helm chart from tonin.toml
```

## Non-negotiable rules

1. **Proto field numbers are immutable** — never renumber; add `reserved N;` instead of removing.
2. **DB migrations are immutable once merged** — never edit a committed `.sql` file.
3. **Only additive changes** to proto and DB schema without a version bump.
4. **Never hand-edit** generated Helm chart files — always re-run `tonin helm generate`.
5. **Never commit** `k8s/secrets.yaml` or `k8s/db-secret.yaml` with real values.
6. All crates: `edition = "2024"`, `license = "LicenseRef-Commercial"`.
7. Backward-compat breakage requires a proto package version bump (`v1` → `v2`).
"#
    )
}

fn ws_gitignore() -> String {
    r#"/target/
Cargo.lock

.DS_Store
Thumbs.db
.idea/
.vscode/
*.swp
*.swo
.env
.env.*
!.env.example
"#
    .to_string()
}

fn proto_cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name        = "{name}-proto"
description = "Generated gRPC types for {name} ({name}.v1)"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
tonic       = {{ workspace = true }}
prost       = {{ workspace = true }}
prost-types = {{ workspace = true }}

[build-dependencies]
tonic-build = "0.12"
"#
    )
}

fn proto_build_rs(snake: &str) -> String {
    format!(
        r#"fn main() -> Result<(), Box<dyn std::error::Error>> {{
    if std::env::var("TONIN_SKIP_PROTOC").is_ok() {{
        return Ok(());
    }}
    let wkt = std::env::var("PROTOC_INCLUDE").ok();
    let mut includes: Vec<&str> = vec!["proto"];
    let wkt_owned;
    if let Some(ref w) = wkt {{
        wkt_owned = w.clone();
        includes.push(&wkt_owned);
    }}
    tonic_build::configure().compile_protos(&["proto/{snake}.proto"], &includes)?;
    Ok(())
}}
"#
    )
}

fn proto_lib_rs(snake: &str) -> String {
    format!("tonic::include_proto!(\"{snake}.v1\");\n")
}

fn server_cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name    = "{name}-server"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "{name}"
path = "src/main.rs"

[lib]
path = "src/lib.rs"

[dependencies]
{name}-proto = {{ workspace = true }}
tonin        = {{ workspace = true }}
tonic        = {{ workspace = true }}
prost        = {{ workspace = true }}
prost-types  = {{ workspace = true }}
tokio        = {{ workspace = true }}
async-trait  = {{ workspace = true }}
tracing      = {{ workspace = true }}
thiserror    = {{ workspace = true }}

[dev-dependencies]
tokio-stream = {{ workspace = true }}
"#
    )
}

fn server_tonin_toml(name: &str) -> String {
    format!(
        r#"schema = "v1"

[service]
name    = "{name}"
version = "0.1.0"
codec   = "prost"

[deploy]
replicas    = 1
mesh        = "cilium"
mcp_sidecar = true
namespace   = "default"

[resources]
cpu    = "100m"
memory = "128Mi"

[autoscale]
max_replicas = 3

[database]
engine = "postgres"

[cache]
engine = "redis"

# Services allowed to call this one (ingress allowlist for CiliumNetworkPolicy).
# Format: <service-name> = "<namespace>"
[callers]
# gateway = "default"
"#
    )
}

fn server_main_rs(name: &str, snake: &str, camel: &str, proto_mod: &str) -> String {
    format!(
        r#"use {snake}_proto::{proto_mod}_server::{camel}Server;
use {snake}_server::{camel}Service;
use tonin::prelude::*;

#[tokio::main]
async fn main() -> tonin::Result<()> {{
    Service::new("{name}")
        .handler({camel}Server::new({camel}Service::default()))
        .run()
        .await
}}
"#
    )
}

fn server_lib_rs(camel: &str) -> String {
    format!(
        r#"pub mod service;
pub use service::{camel}Service;
"#
    )
}

fn server_service_rs(snake: &str, camel: &str, proto_mod: &str) -> String {
    format!(
        r#"use tonic::{{Request, Response, Status}};
use {snake}_proto::{{{proto_mod}_server::{camel}, HelloReply, HelloRequest}};

/// `{camel}` gRPC service — stub implementation.
/// Replace each `unimplemented` return with real logic.
#[derive(Debug, Default)]
pub struct {camel}Service;

#[tonic::async_trait]
impl {camel} for {camel}Service {{
    async fn say_hello(
        &self,
        _req: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {{
        Err(Status::unimplemented("stub: implement say_hello"))
    }}
}}
"#
    )
}

fn server_contract_test(snake: &str, camel: &str, proto_mod: &str) -> String {
    format!(
        r#"//! Contract tests — verifies all RPCs are reachable.
//! Starts an in-process tonic server; all stubs return UNIMPLEMENTED.

use std::time::Duration;
use {snake}_proto::{{{proto_mod}_client::{camel}Client, {proto_mod}_server::{camel}Server, HelloRequest}};
use {snake}_server::{camel}Service;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;

async fn start_server() -> String {{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {{
        tonic::transport::Server::builder()
            .add_service({camel}Server::new({camel}Service::default()))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    }});
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("http://{{addr}}")
}}

#[tokio::test]
async fn test_say_hello_stub_returns_unimplemented() {{
    let uri = start_server().await;
    let mut client = {camel}Client::connect(uri).await.unwrap();
    let err = client
        .say_hello(HelloRequest {{ name: "test".into() }})
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented);
}}
"#
    )
}

fn server_gitignore() -> String {
    r#"/target/
.env
.env.*
!.env.example

# tonin: generated secret manifests — never commit plaintext values
k8s/secrets.yaml
k8s/db-secret.yaml
"#
    .to_string()
}

fn rs_cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name        = "{name}-rs"
description = "Rust client for the {name} service — pre-wired with tonin-client coalescing"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
{name}-proto = {{ workspace = true }}
tonin-client = {{ workspace = true }}
tonic        = {{ workspace = true }}
tokio        = {{ workspace = true }}
tracing      = {{ workspace = true }}
"#
    )
}

fn rs_lib_rs(name: &str, snake: &str, camel: &str, proto_mod: &str) -> String {
    format!(
        r#"//! Rust client for the {name} service.
//!
//! ```no_run
//! use {snake}_rs::{camel}Client;
//! use tonic::transport::Endpoint;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {{
//! let ep = Endpoint::from_static("http://{name}.default.svc.cluster.local:50051");
//! let client = {camel}Client::connect(ep).await?;
//! // client.inner.say_hello(...).await?;
//! # Ok(())
//! # }}
//! ```

pub use {snake}_proto as proto;

use {snake}_proto::{proto_mod}_client::{camel}Client as Raw{camel}Client;
use tonin_client::client::CoalescingClient;
use tonic::transport::Channel;

/// Pre-wired {camel} service client with singleflight coalescing (default-on).
/// Wrap in `Arc` to share cheaply across tasks.
pub struct {camel}Client {{
    channel: Channel,
}}

impl {camel}Client {{
    /// Connect to the {name} service.
    pub async fn connect(
        endpoint: tonic::transport::Endpoint,
    ) -> Result<Self, tonic::transport::Error> {{
        let channel = endpoint.connect().await?;
        Ok(Self {{ channel }})
    }}

    /// Returns the gRPC client wrapped in `CoalescingClient`.
    pub fn inner(&self) -> CoalescingClient<Raw{camel}Client<Channel>> {{
        CoalescingClient::new(Raw{camel}Client::new(self.channel.clone()))
    }}
}}
"#
    )
}

fn sub_claude_md(
    name: &str,
    crate_kind: &str,
    snake: &str,
    camel: &str,
    proto_mod: &str,
) -> String {
    let header = format!("@../CLAUDE.md\n\n# {name}-{crate_kind} — Claude guidance\n");
    let body = match crate_kind {
        "proto" => format!(
            r#"
Contract crate. One proto file → generated Rust types consumed by every other crate.

## Commands

```bash
cargo build -p {name}-proto          # runs tonic-build codegen
TONIN_SKIP_PROTOC=1 cargo check      # skip codegen when protoc unavailable
```

## Proto versioning — backward compatibility

Package `{snake}.v1`. Bump to `v2` only when a wire-breaking change is unavoidable.

### Safe (additive — no version bump needed)
```proto
// Add a new field — always use a fresh number, never reuse a deleted one
string new_field = 5;

// Add a new RPC
rpc NewMethod (NewRequest) returns (NewResponse);

// Add a new enum value
enum Status {{ UNKNOWN = 0; ACTIVE = 1; NEW_VALUE = 2; }}
```

### Unsafe — requires `v2` package
```proto
// NEVER do any of these in v1:
// - Renumber: string name = 1; → string name = 2;   (wire breaking)
// - Change type: string id = 1; → int64 id = 1;     (wire breaking)
// - Remove a field without reserving its number
```

### Deprecating a field (safe alternative to removal)
```proto
string old_field = 3 [deprecated = true];
// After all clients have migrated:
reserved 3;
reserved "old_field";
```

## Rust codegen paths

| Proto element | Generated Rust |
|---------------|----------------|
| `service {camel}` | `{proto_mod}_server::{camel}` (trait) + `{proto_mod}_server::{camel}Server` (wrapper) |
| `service {camel}` | `{proto_mod}_client::{camel}Client` (tonic client) |
| `message HelloRequest` | `{snake}_proto::HelloRequest` |
| `google.protobuf.Timestamp` | `prost_types::Timestamp` |
| `google.protobuf.Empty` RPC return | `()` in tonic trait |
"#
        ),
        "server" => format!(
            r#"
tonin gRPC binary. Handlers start as stubs (`UNIMPLEMENTED`); fill in real logic milestone by milestone.

## Commands

```bash
cargo build -p {name}-server
cargo test --test contract_e2e_test   # in-process gRPC, no Docker needed
cargo test -p {name}-server           # all tests including unit tests
tonin helm generate                   # render Helm chart from tonin.toml
```

## Rust / tonin coding standards

- **Async-first.** All I/O is async. Never block a tokio thread (`std::fs`, `std::net`, CPU loops).
  Use `tokio::fs`, `tokio::net`, or `tokio::task::spawn_blocking` for blocking work.
- **No panics in handler code.** Return `Result<Response<T>, Status>`. Map errors:
  ```rust
  .map_err(|e| Status::internal(format!("db error: {{e}}")))?
  ```
- **No locks across `.await`.** Clone data out of a `Mutex`/`RwLock` before awaiting.
- **`#[tonic::async_trait]`** on every `impl {camel} for {camel}Service` block.
- **clippy pedantic** — `cargo clippy -- -D warnings` must be clean before merging.

## Implementing a stub handler

```rust
// Before: stub
async fn say_hello(&self, _req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {{
    Err(Status::unimplemented("stub"))
}}

// After: real implementation
async fn say_hello(&self, req: Request<HelloRequest>) -> Result<Response<HelloReply>, Status> {{
    let inner = req.into_inner();
    // 1. validate input
    // 2. call service layer (inject via self.state)
    // 3. return response
    Ok(Response::new(HelloReply {{ message: format!("hello {{}}", inner.name) }}))
}}
```

## Test rules

| Scope | Location | Infrastructure |
|-------|----------|----------------|
| Handler logic, error paths | Inline `#[cfg(test)] mod tests {{}}` | Mock deps |
| All RPCs reachable | `tests/contract_e2e_test.rs` | In-process tonic server |
| Migrations apply cleanly | `tests/migrations_test.rs` (add when ready) | testcontainers Postgres |
| Tenant isolation | E2E (add when authz is live) | Real DB |

Test naming: `test_<function>_<scenario>_<expected>`.

## DB migration safety

Files in `migrations/` are **immutable once merged**. Never edit a committed file.

```sql
-- SAFE: always provide a DEFAULT so existing rows are not broken
ALTER TABLE foo ADD COLUMN bar TEXT NOT NULL DEFAULT '';

-- SAFE: non-blocking index creation
CREATE INDEX CONCURRENTLY idx_foo_bar ON foo(bar);

-- UNSAFE: dropping a column requires two deploys
-- Deploy 1: stop reading/writing the column in code
-- Deploy 2: ALTER TABLE foo DROP COLUMN bar;  (new migration file)
```
"#
        ),
        "rs" => format!(
            r#"
Rust client library. Any service calling `{name}` depends on this crate only —
never import `{name}-proto` or `tonic` directly in callers.

## Usage

```rust
use {snake}_rs::{camel}Client;
use tonic::transport::Endpoint;

// Construct once; wrap in Arc to share across tasks.
let ep = Endpoint::from_static("http://{name}.default.svc.cluster.local:50051");
let client = std::sync::Arc::new({camel}Client::connect(ep).await?);

// Each call returns a CoalescingClient-wrapped tonic client.
let mut c = client.inner();
let reply = c.say_hello({snake}_proto::HelloRequest {{ name: "world".into() }}).await?;
```

## Rust standards for this crate

- **Thin wrapper only.** No business logic, no DB access, no inbound auth middleware.
- **Error propagation.** Return `Result<T, tonic::Status>` — never swallow errors.
- **No retry logic.** The mesh (Cilium/Istio) handles retries. `CoalescingClient` handles
  deduplication. Adding retry here creates double-retry bugs.
- **`Arc<{camel}Client>`** — the `{camel}Client` struct holds a `Channel` (internally ref-counted),
  so wrapping in `Arc` is the canonical share-across-tasks pattern.

## Adding a convenience method

```rust
impl {camel}Client {{
    /// One-line helper that hides the inner() unwrap from callers.
    pub async fn say_hello(&self, name: impl Into<String>) -> Result<{snake}_proto::HelloReply, tonic::Status> {{
        self.inner()
            .say_hello({snake}_proto::HelloRequest {{ name: name.into() }})
            .await
            .map(|r| r.into_inner())
    }}
}}
```

Keep helpers thin — no caching, no validation, no retry.
"#
        ),
        _ => String::new(),
    };
    format!("{header}{body}")
}

fn ws_client_claude_md(name: &str, suffix: &str, vars: &Vars) -> String {
    let camel = &vars.service_camel;
    let snake = &vars.service_name_snake;
    match suffix {
        "py" => format!(
            r#"@../CLAUDE.md

# {name}-py — Claude guidance

Python gRPC client for the `{name}` service.

## Commands

```bash
bash codegen.sh          # regenerate _pb/ stubs from ../{name}-proto/proto/
uv sync                  # install / sync deps
uv run python -c "from {snake}_client import {camel}Stub"   # smoke-test import
```

**Do not commit `src/{snake}_client/_pb/`** — it is gitignored and regenerated by `codegen.sh`.
Run `codegen.sh` whenever `../{name}-proto/proto/{snake}.proto` changes.

## Usage

```python
from __future__ import annotations
import grpc.aio
from {snake}_client import {camel}Stub, HelloRequest, AuthCtx

async def call(token: str) -> None:
    async with grpc.aio.insecure_channel("{name}.default.svc.cluster.local:50051") as ch:
        stub = {camel}Stub(ch)
        metadata: list[tuple[str, str]] = []
        AuthCtx.from_bearer(token).propagate(metadata)
        reply = await stub.SayHello(HelloRequest(name="world"), metadata=metadata)
        print(reply.message)
```

## Python coding standards

- **Type hints everywhere.** Every function signature must be fully annotated.
  `from __future__ import annotations` at the top of every file.
- **`grpc.aio` only** — never use synchronous `grpc` in async contexts.
- **`async with` for channels** — always close channels when done; use context managers.
- **Explicit error handling.** Catch `grpc.aio.AioRpcError`, check `.code()`,
  never swallow errors with bare `except Exception: pass`.
- **No business logic here.** This crate is a thin client wrapper only.

## Versioning

Package name: `{name}-py`. Semver tracks the server MAJOR version.
When the proto bumps to `v2`, this package bumps to `2.0.0`.

## Proto stubs versioning

`_pb/` stubs are derived from `{name}-proto`. When the proto adds a field or RPC:
1. The field/RPC is automatically available after re-running `codegen.sh`.
2. Old stubs still work (protobuf backward compatibility) — unknown fields are ignored.
3. Removing or renumbering a field in proto → update this client at the same time.
"#
        ),
        "ts" => format!(
            r#"@../CLAUDE.md

# {name}-ts — Claude guidance

TypeScript gRPC client for the `{name}` service (Node + grpc-web compatible).

## Commands

```bash
npm install              # install deps
npm run gen              # regenerate src/gen/ from ../{name}-proto/proto/
npm run build            # compile TypeScript → dist/
```

**Do not commit `src/gen/`** — it is gitignored and regenerated by `npm run gen`.
Run `npm run gen` whenever `../{name}-proto/proto/{snake}.proto` changes.

## Usage (Node / server-side)

```typescript
import {{ credentials, ServiceError }} from "@grpc/grpc-js";
import {{ {camel}Client }} from "./{snake}";  // from src/gen/

const client = new {camel}Client(
  "{name}.default.svc.cluster.local:50051",
  credentials.createInsecure(),
);

client.sayHello({{ name: "world" }}, (err: ServiceError | null, res) => {{
  if (err) throw err;
  console.log(res.message);
}});
```

## TypeScript coding standards

- **Strict mode.** `tsconfig.json` sets `strict: true` — never disable it.
  No `any`, no `@ts-ignore`, no `as unknown as T`.
- **`undefined` over `null`.** Use optional chaining (`?.`) and nullish coalescing (`??`).
- **Explicit return types** on all exported functions and class methods.
- **`async`/`await` over callbacks** — wrap grpc callbacks in promises:
  ```typescript
  const reply = await new Promise<HelloReply>((resolve, reject) =>
    client.sayHello({{ name: "world" }}, (err, res) => err ? reject(err) : resolve(res))
  );
  ```
- **ESM modules** (`"type": "module"` in package.json). Always use `.js` extensions
  in import paths even when importing `.ts` files (tsc/Node ESM requirement).
- **No business logic here.** This package is a thin client wrapper only.

## Versioning

Package name: `{name}-ts`. Semver tracks the server MAJOR version.
When the proto bumps to `v2`, this package bumps to `2.0.0`.

## Proto stubs versioning

`src/gen/` is derived from `{name}-proto` via `buf`. When the proto adds a field or RPC:
1. Run `npm run gen` — new fields/RPCs appear automatically.
2. Old code still compiles (protobuf adds fields, never removes in a compatible change).
3. If a field is removed from proto → update this client in the same PR.
"#
        ),
        _ => format!("@../CLAUDE.md\n\n# {name}-{suffix}\n"),
    }
}

fn ws_client_gitignore(client: ClientLang) -> String {
    match client {
        ClientLang::Python => r#"# Python
__pycache__/
*.py[cod]
*.egg-info/
dist/
.venv/

# Generated proto stubs — regenerate with codegen.sh
src/*/_pb/
"#
        .to_string(),
        ClientLang::Ts => r#"# Node / TypeScript
node_modules/
dist/

# Generated proto stubs — regenerate with npm run gen
src/gen/
"#
        .to_string(),
        ClientLang::Rust => String::new(),
    }
}

fn ws_makefile(name: &str) -> String {
    format!(
        ".DEFAULT_GOAL := help\n\
         \n\
         .PHONY: help install build check fmt fmt-check lint test test-e2e e2e ci k8s-generate clean \\\n\
         \t\tproto server rs\n\
         \n\
         # ── help ─────────────────────────────────────────────────────────────────────\n\
         help:\n\
         \t@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \\\n\
         \t  awk 'BEGIN {{FS = \":.*?## \"}}; {{printf \"  %-18s %s\\n\", $$1, $$2}}'\n\
         \n\
         # ── setup ─────────────────────────────────────────────────────────────────────\n\
         install: ## Verify required tools are installed (cargo, uv, npm)\n\
         \t@command -v cargo >/dev/null 2>&1 || (echo \"cargo not found\" && exit 1)\n\
         \t@command -v uv >/dev/null 2>&1 || (echo \"uv not found. Install: pip install uv\" && exit 1)\n\
         \t@command -v npm >/dev/null 2>&1 || (echo \"npm not found. Install Node.js from nodejs.org\" && exit 1)\n\
         \t@echo \"✓ All required tools found: cargo, uv, npm\"\n\
         \n\
         # ── workspace-wide ────────────────────────────────────────────────────────────\n\
         build: ## Build all crates (debug)\n\
         \tcargo build --workspace\n\
         \n\
         check: ## Type-check all crates + all targets\n\
         \tcargo check --workspace --all-targets\n\
         \n\
         fmt: ## Format all code\n\
         \tcargo fmt --all\n\
         \n\
         fmt-check: ## Check formatting (CI)\n\
         \tcargo fmt --all -- --check\n\
         \n\
         lint: ## Clippy -D warnings across workspace\n\
         \tcargo clippy --workspace --all-targets -- -D warnings\n\
         \n\
         test: ## Unit + contract tests (no Docker required)\n\
         \tcargo nextest run --workspace\n\
         \n\
         test-e2e: ## Run all E2E tests in {name}-e2e/ (requires Docker)\n\
         \t$(MAKE) -C {name}-e2e test\n\
         \n\
         e2e: ## Run all E2E tests in {name}-e2e/ (requires Docker)\n\
         \t$(MAKE) -C {name}-e2e test\n\
         \n\
         ci: fmt-check lint test ## Full CI gate (fmt + lint + nextest)\n\
         \n\
         k8s-generate: ## Regenerate k8s manifests from tonin.toml\n\
         \t$(MAKE) -C {name}-server k8s-generate\n\
         \n\
         clean: ## Remove build artifacts\n\
         \tcargo clean\n\
         \n\
         # ── per-crate delegates ───────────────────────────────────────────────────────\n\
         proto: ## Run ci in {name}-proto/\n\
         \t$(MAKE) -C {name}-proto ci\n\
         \n\
         server: ## Run ci in {name}-server/\n\
         \t$(MAKE) -C {name}-server ci\n\
         \n\
         rs: ## Run ci in {name}-rs/\n\
         \t$(MAKE) -C {name}-rs ci\n"
    )
}

fn proto_makefile(name: &str) -> String {
    format!(
        ".DEFAULT_GOAL := help\n\
         \n\
         .PHONY: help build check fmt fmt-check lint ci\n\
         \n\
         help:\n\
         \t@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \\\n\
         \t  awk 'BEGIN {{FS = \":.*?## \"}}; {{printf \"  %-16s %s\\n\", $$1, $$2}}'\n\
         \n\
         build: ## Compile {name}-proto (runs protoc)\n\
         \tcargo build -p {name}-proto\n\
         \n\
         check: ## Type-check without codegen\n\
         \tTONIN_SKIP_PROTOC=1 cargo check -p {name}-proto\n\
         \n\
         fmt: ## Format\n\
         \tcargo fmt -p {name}-proto\n\
         \n\
         fmt-check: ## Check formatting (CI)\n\
         \tcargo fmt -p {name}-proto -- --check\n\
         \n\
         lint: ## Clippy\n\
         \tcargo clippy -p {name}-proto -- -D warnings\n\
         \n\
         ci: fmt-check lint build ## fmt + lint + build\n"
    )
}

fn server_makefile(name: &str) -> String {
    format!(
        ".DEFAULT_GOAL := help\n\
         \n\
         .PHONY: help build check fmt fmt-check lint test test-contract test-migrations \\\n\
         \t\tk8s-generate migrate ci\n\
         \n\
         help:\n\
         \t@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \\\n\
         \t  awk 'BEGIN {{FS = \":.*?## \"}}; {{printf \"  %-20s %s\\n\", $$1, $$2}}'\n\
         \n\
         build: ## Build {name}-server binary\n\
         \tcargo build -p {name}-server\n\
         \n\
         check: ## Type-check all targets\n\
         \tcargo check -p {name}-server --all-targets\n\
         \n\
         fmt: ## Format\n\
         \tcargo fmt -p {name}-server\n\
         \n\
         fmt-check: ## Check formatting (CI)\n\
         \tcargo fmt -p {name}-server -- --check\n\
         \n\
         lint: ## Clippy -D warnings\n\
         \tcargo clippy -p {name}-server -- -D warnings\n\
         \n\
         test: ## All tests (contract only — no Docker)\n\
         \tcargo nextest run --test contract_e2e_test\n\
         \n\
         test-contract: ## In-process gRPC contract smoke tests\n\
         \tcargo nextest run --test contract_e2e_test\n\
         \n\
         test-migrations: ## Migrations test (requires Docker / Rancher Desktop)\n\
         \tcargo nextest run --test migrations_test\n\
         \n\
         k8s-generate: ## Regenerate k8s manifests via tonin\n\
         \ttonin helm generate\n\
         \n\
         migrate: ## Apply migrations to DATABASE_URL (dev/local)\n\
         \tsqlx migrate run --source migrations\n\
         \n\
         ci: fmt-check lint test ## fmt + lint + nextest\n"
    )
}

fn rs_makefile(name: &str) -> String {
    format!(
        ".DEFAULT_GOAL := help\n\
         \n\
         .PHONY: help build check fmt fmt-check lint test ci\n\
         \n\
         help:\n\
         \t@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \\\n\
         \t  awk 'BEGIN {{FS = \":.*?## \"}}; {{printf \"  %-16s %s\\n\", $$1, $$2}}'\n\
         \n\
         build: ## Build {name}-rs\n\
         \tcargo build -p {name}-rs\n\
         \n\
         check: ## Type-check all targets\n\
         \tcargo check -p {name}-rs --all-targets\n\
         \n\
         fmt: ## Format\n\
         \tcargo fmt -p {name}-rs\n\
         \n\
         fmt-check: ## Check formatting (CI)\n\
         \tcargo fmt -p {name}-rs -- --check\n\
         \n\
         lint: ## Clippy -D warnings\n\
         \tcargo clippy -p {name}-rs -- -D warnings\n\
         \n\
         test: ## Unit + doc tests\n\
         \tcargo nextest run -p {name}-rs\n\
         \n\
         ci: fmt-check lint test ## fmt + lint + nextest\n"
    )
}

fn ws_github_ci_yml(name: &str) -> String {
    format!(
        "name: CI\n\
         \n\
         on:\n\
         \
           push:\n\
         \
             branches: [main]\n\
         \
           pull_request:\n\
         \
             branches: [main]\n\
         \n\
         env:\n\
         \
           CARGO_TERM_COLOR: always\n\
         \
           PROTOC_INCLUDE: /usr/local/include\n\
         \
           RUSTFLAGS: \"-D warnings\"\n\
         \n\
         jobs:\n\
         \
           ci:\n\
         \
             name: fmt · lint · test\n\
         \
             runs-on: ubuntu-latest\n\
         \
             steps:\n\
         \
               - uses: actions/checkout@v4\n\
         \n\
         \
               - name: Install Rust toolchain\n\
         \
                 uses: dtolnay/rust-toolchain@stable\n\
         \
                 with:\n\
         \
                   components: rustfmt, clippy\n\
         \n\
         \
               - name: Install protoc\n\
         \
                 uses: arduino/setup-protoc@v3\n\
         \
                 with:\n\
         \
                   repo-token: ${{{{ secrets.GITHUB_TOKEN }}}}\n\
         \n\
         \
               - name: Cache cargo registry + target\n\
         \
                 uses: actions/cache@v4\n\
         \
                 with:\n\
         \
                   path: |\n\
         \
                     ~/.cargo/registry\n\
         \
                     ~/.cargo/git\n\
         \
                     /tmp/{name}-target\n\
         \
                   key: ${{{{ runner.os }}}}-cargo-${{{{ hashFiles('**/Cargo.lock') }}}}\n\
         \
                   restore-keys: ${{{{ runner.os }}}}-cargo-\n\
         \n\
         \
               - name: Check formatting\n\
         \
                 run: make fmt-check\n\
         \n\
         \
               - name: Lint\n\
         \
                 run: make lint\n\
         \n\
         \
               - name: Test (no Docker)\n\
         \
                 run: make test\n\
         \n\
           migrations:\n\
         \
             name: migrations test\n\
         \
             runs-on: ubuntu-latest\n\
         \
             services:\n\
         \
               postgres:\n\
         \
                 image: postgres:17\n\
         \
                 env:\n\
         \
                   POSTGRES_USER: postgres\n\
         \
                   POSTGRES_PASSWORD: postgres\n\
         \
                   POSTGRES_DB: {name}_test\n\
         \
                 ports: [\"5432:5432\"]\n\
         \
                 options: >-\n\
         \
                   --health-cmd pg_isready\n\
         \
                   --health-interval 5s\n\
         \
                   --health-timeout 5s\n\
         \
                   --health-retries 10\n\
         \
             steps:\n\
         \
               - uses: actions/checkout@v4\n\
         \n\
         \
               - name: Install Rust toolchain\n\
         \
                 uses: dtolnay/rust-toolchain@stable\n\
         \n\
         \
               - name: Install protoc\n\
         \
                 uses: arduino/setup-protoc@v3\n\
         \
                 with:\n\
         \
                   repo-token: ${{{{ secrets.GITHUB_TOKEN }}}}\n\
         \n\
         \
               - name: Cache cargo\n\
         \
                 uses: actions/cache@v4\n\
         \
                 with:\n\
         \
                   path: |\n\
         \
                     ~/.cargo/registry\n\
         \
                     ~/.cargo/git\n\
         \
                     /tmp/{name}-target\n\
         \
                   key: ${{{{ runner.os }}}}-cargo-${{{{ hashFiles('**/Cargo.lock') }}}}\n\
         \
                   restore-keys: ${{{{ runner.os }}}}-cargo-\n\
         \n\
         \
               - name: Run migrations test\n\
         \
                 env:\n\
         \
                   DATABASE_URL: postgres://postgres:postgres@localhost:5432/{name}_test\n\
         \
                   DOCKER_HOST: unix:///var/run/docker.sock\n\
         \
                 run: make test-e2e\n"
    )
}

// ─────────────────────────────────────────────────────────────────────────
// E2E crate content helpers
// ─────────────────────────────────────────────────────────────────────────

fn e2e_cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name    = "{name}-e2e"
version.workspace = true
edition.workspace = true
license.workspace = true
publish = false

# No src/ — this package contains only integration tests in tests/.
# Each file under tests/ is a separate test binary that starts a real
# server against testcontainer Postgres + Redis and connects via {name}-rs.

[dev-dependencies]
{name}-proto  = {{ workspace = true }}
{name}-rs     = {{ workspace = true }}
{name}-server = {{ path = "../{name}-server" }}
tonic           = {{ workspace = true }}
tokio           = {{ workspace = true }}
tokio-stream    = {{ workspace = true }}
testcontainers         = {{ workspace = true }}
testcontainers-modules = {{ workspace = true }}
sqlx    = {{ workspace = true }}
anyhow  = {{ workspace = true }}
"#
    )
}

fn e2e_makefile(name: &str) -> String {
    format!(
        ".DEFAULT_GOAL := help\n\
         \n\
         .PHONY: help check test ci\n\
         \n\
         help:\n\
         \t@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \\\n\
         \t  awk 'BEGIN {{FS = \":.*?## \"}}; {{printf \"  %-18s %s\\n\", $$1, $$2}}'\n\
         \n\
         check: ## Type-check without Docker\n\
         \tcargo check -p {name}-e2e --tests\n\
         \n\
         test: ## All E2E tests (requires Docker)\n\
         \tcargo test -p {name}-e2e\n\
         \n\
         ci: test ## E2E CI gate (requires Docker)\n"
    )
}

fn e2e_common_mod(name: &str) -> String {
    let snake = name.replace('-', "_");
    let camel = {
        use convert_case::{Case, Casing};
        name.to_case(Case::Pascal)
    };
    format!(
        r#"//! Shared E2E harness: testcontainer Postgres + Redis + in-process {name} server.
//!
//! Include in each test file with:
//!   #[path = "common/mod.rs"] mod common;

use std::net::TcpListener;
use testcontainers::{{ContainerAsync, ImageExt, runners::AsyncRunner}};
use testcontainers_modules::{{postgres::Postgres, redis::Redis}};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Endpoint;

use {snake}_proto::{snake}_server::{camel}Server;
use {snake}_server::{camel}Service;
use {snake}_rs::{camel}Client;

/// Full E2E fixture: Postgres + Redis containers + real in-process server + connected client.
///
/// Keep the returned `E2EHarness` alive for the duration of the test — dropping it stops
/// the containers.
pub struct E2EHarness {{
    pub client: {camel}Client,
    pub db_url: String,
    pub redis_url: String,
    _pg: ContainerAsync<Postgres>,
    _redis: ContainerAsync<Redis>,
}}

impl E2EHarness {{
    pub async fn start() -> anyhow::Result<Self> {{
        // Start containers
        let pg = Postgres::default()
            .with_db_name("{snake}_e2e")
            .with_user("postgres")
            .with_password("postgres")
            .start()
            .await?;
        let redis = Redis::default().start().await?;

        let pg_port = pg.get_host_port_ipv4(5432).await?;
        let redis_port = redis.get_host_port_ipv4(6379).await?;

        let db_url = format!("postgres://postgres:postgres@127.0.0.1:{{pg_port}}/{snake}_e2e");
        let redis_url = format!("redis://127.0.0.1:{{redis_port}}");

        // Apply migrations from the server crate (path relative to this crate's manifest)
        let migrations = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../{name}-server/migrations");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&db_url)
            .await?;
        sqlx::migrate::Migrator::new(migrations)
            .await?
            .run(&pool)
            .await?;

        // Bind a random port; start the tonic server in a background task
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let incoming =
            TcpListenerStream::new(tokio::net::TcpListener::from_std(listener)?);

        // TODO(M2/M3): pass db_url + redis_url into service constructors via State
        let _db = db_url.clone();
        let _redis = redis_url.clone();

        tokio::spawn(async move {{
            tonic::transport::Server::builder()
                .add_service({camel}Server::new({camel}Service::default()))
                .serve_with_incoming(incoming)
                .await
                .expect("e2e server panicked");
        }});

        // Connect via {name}-rs (the public client crate)
        let endpoint = Endpoint::from_shared(format!("http://{{addr}}"))?;
        let client = {camel}Client::connect(endpoint).await?;

        Ok(Self {{
            client,
            db_url,
            redis_url,
            _pg: pg,
            _redis: redis,
        }})
    }}
}}

/// Returns true if Docker is reachable.
/// Set DOCKER_HOST=unix:///home/<user>/.rd/docker.sock for Rancher Desktop.
pub fn docker_available() -> bool {{
    let sock = std::env::var("DOCKER_HOST")
        .unwrap_or_else(|_| "unix:///var/run/docker.sock".to_string());
    let path = sock.strip_prefix("unix://").unwrap_or(&sock);
    std::path::Path::new(path).exists()
}}

/// Macro: skip the test with a message when Docker is not available.
#[macro_export]
macro_rules! require_docker {{
    () => {{
        if !common::docker_available() {{
            eprintln!("skip: Docker not available — set DOCKER_HOST or start Docker");
            return Ok(());
        }}
    }};
}}
"#,
    )
}

fn e2e_contract_test(name: &str) -> String {
    let snake = name.replace('-', "_");
    let camel = {
        use convert_case::{Case, Casing};
        name.to_case(Case::Pascal)
    };
    format!(
        r#"//! Contract smoke test — verifies the server is reachable and the gRPC
//! transport works end-to-end. Individual service-specific test files
//! (e.g. tests/service.rs) are added as each RPC is implemented.
//!
//! This file just checks "server up + client connects + any RPC call
//! returns a gRPC status (not a connection/transport error)".

#[path = "common/mod.rs"]
mod common;

use {snake}_proto::HelloRequest;

type Result = anyhow::Result<()>;

#[tokio::test]
async fn test_{snake}_e2e_server_reachable() -> Result {{
    require_docker!();
    let h = common::E2EHarness::start().await?;

    // The harness already connected the client in E2EHarness::start().
    // Any connection-level failure (port unreachable, TLS) would have
    // panicked there. Call say_hello via the CoalescingClient — the stub
    // returns UNIMPLEMENTED which is a valid gRPC status, confirming the
    // server is up and the transport layer is healthy.
    //
    // CoalescingClient::inner is the raw tonic-generated client; calling
    // it directly avoids needing a convenience wrapper on {camel}Client.
    let coalescing = h.client.inner();
    let status = coalescing
        .inner
        .say_hello(HelloRequest {{ name: "smoke".into() }})
        .await
        .unwrap_err();

    // Any gRPC status code is acceptable here — the important thing is that
    // we did NOT get a transport error (which would be tonic::Code::Unknown
    // with a "connection refused" or "transport error" message).
    assert_ne!(
        status.code(),
        tonic::Code::Unknown,
        "transport error — is the server running? {{status}}",
    );
    Ok(())
}}
"#,
    )
}

fn print_workspace_next_steps(name: &str, extras: &[ClientLang]) {
    eprintln!();
    eprintln!("next steps:");
    eprintln!("  cd {name}");
    eprintln!("  cargo build --workspace          # compiles Rust crates");
    eprintln!("  cargo test --workspace           # contract test should pass");
    eprintln!("  cd {name}-server && tonin helm generate  # render Helm chart from tonin.toml");
    for client in extras {
        match client {
            ClientLang::Python => {
                eprintln!("  (cd {name}-py && bash codegen.sh)   # generate Python stubs")
            }
            ClientLang::Ts => {
                eprintln!("  (cd {name}-ts && npm install && npm run gen)  # generate TS stubs")
            }
            ClientLang::Rust => {}
        }
    }
    eprintln!();
    eprintln!("scaffold:");
    eprintln!("  {name}-proto/   ← proto contract + generated types");
    eprintln!("  {name}-server/  ← tonin gRPC binary (stubs, fill in real logic)");
    eprintln!("  {name}-rs/      ← Rust client library for callers");
    for client in extras {
        match client {
            ClientLang::Python => {
                eprintln!("  {name}-py/      ← Python client (gRPC stubs + tonin_client)")
            }
            ClientLang::Ts => {
                eprintln!("  {name}-ts/      ← TypeScript client (buf-generated stubs)")
            }
            ClientLang::Rust => {}
        }
    }
}
