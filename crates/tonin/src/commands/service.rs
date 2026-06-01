//! `tonin service ...` — service lifecycle.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Subcommand, ValueEnum};

use super::new;

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Scaffold a new service.
    ///
    /// Creates `./<name>/` with proto, src, `tonin.toml`, Dockerfile,
    /// and pre-generated `k8s/` manifests. The exact tree depends on
    /// `--lang` and `--type` — Rust scaffolds a multi-crate workspace
    /// (`server/` + `client-rust/`); Python lands a `server/` package;
    /// TypeScript lands a Vite SPA or Next.js BFF tree.
    ///
    /// Idempotency: refuses to overwrite an existing `./<name>/`. Delete
    /// it (or pick a different name) before re-running.
    #[command(after_long_help = "PREREQUISITES:
  - Write access to the current directory
  - For `--lang rust`, a Rust toolchain in PATH (used for cargo new under
    the hood)
  - For `--lang python`, no system deps required at scaffold time; the
    generated service brings its own pyproject.toml
  - For `--lang ts`, no system deps required at scaffold time; the
    generated service brings its own package.json

SIDE EFFECTS:
  - Creates ./<name>/ with the full service tree
  - May prompt to add the new crate to a parent Cargo workspace if it
    detects one (skip with --no-workspace)

EXAMPLES:
  # The minimum — a Rust backend service
  tonin service new greeter

  # Python backend service with a Rust client SDK alongside
  tonin service new notifier --lang python --client-lang rust

  # TypeScript SPA frontend
  tonin service new dashboard --lang ts --type web --web-mode spa

  # Next.js BFF that proxies to backend services
  tonin service new web-bff --lang ts --type web --web-mode bff

  # Backend with two background jobs and S3 storage
  tonin service new orders --with-job reconcile --with-job dunning --with-storage s3

  # Rust server that also emits Python + TS client packages
  tonin service new greeter --lang rust --client-lang python --client-lang ts

SEE ALSO:
  docs/03-grpc-service.md, docs/15-multi-language.md")]
    New {
        name: String,
        /// Target language. All scaffolds are async-by-default.
        #[arg(long, value_enum, default_value_t = Lang::Rust)]
        lang: Lang,
        /// Service shape. Only meaningful for `--lang ts`.
        #[arg(
            long,
            value_enum,
            long_help = "Service shape. Only meaningful for `--lang ts`:

  --type web      Vite SPA OR Next.js BFF (see --web-mode) — default for ts
  --type backend  Node + ConnectRPC service

For --lang rust and --lang python, `backend` is the only sensible value
and is applied automatically."
        )]
        r#type: Option<ServiceType>,
        /// Web template shape. Only valid with `--lang ts --type web`.
        #[arg(
            long,
            value_enum,
            long_help = "Web template shape. Only valid with `--lang ts --type web`:

  --web-mode spa  Vite + React; pure client-side; served as static files
                  by nginx. Default for --type web.
  --web-mode bff  Next.js (Backend-for-Frontend); the Node server proxies
                  and aggregates declared backend services."
        )]
        web_mode: Option<WebMode>,
        /// Skip the interactive workspace prompt (treat as 'no').
        #[arg(long)]
        no_workspace: bool,
        /// Add a background-job binary. Repeatable. Each job gets its
        /// own `src/bin/<name>.rs` entry point that bootstraps via
        /// `tonin::job::bootstrap(...)` — telemetry + service-identity
        /// auth + State, no gRPC server. Rust scaffolds only.
        #[arg(long = "with-job", value_name = "NAME")]
        with_jobs: Vec<String>,
        /// Wire object storage into State (opendal-backed).
        #[arg(
            long = "with-storage",
            value_enum,
            long_help = "Wire object storage into State (opendal-backed). The chosen
backend determines feature flags and env-var contract:

  --with-storage s3      STORAGE_BUCKET, STORAGE_REGION,
                         STORAGE_ENDPOINT (optional),
                         STORAGE_ACCESS_KEY, STORAGE_SECRET_KEY
  --with-storage gcs     STORAGE_BUCKET, STORAGE_CREDENTIAL_PATH
  --with-storage azure   STORAGE_CONTAINER, STORAGE_ACCOUNT,
                         STORAGE_ACCESS_KEY
  --with-storage local   STORAGE_ROOT (path on disk)

The framework runs a LIST-limit-1 probe at boot; if it fails, the
service refuses to start."
        )]
        with_storage: Option<StorageKind>,
        /// Emit additional client SDKs as sibling folders alongside the
        /// primary one. Repeatable. The server's own native client is
        /// always emitted (determined by `--lang`); these are extras.
        /// Each gets its own package: `client-python/` or `client-ts/`.
        ///
        /// Example — a Rust server with both Rust and Python SDKs:
        ///   tonin service new greeter --lang rust --client-lang python
        #[arg(long = "client-lang", value_enum)]
        client_langs: Vec<ClientLang>,
    },
    /// Run the service (and its MCP sidecar) locally via docker-compose.
    ///
    /// **Stub today** — full implementation lands when the local dev-loop
    /// design ships (see docs/00-overview.md). Use `cargo run -p <service>`
    /// in the meantime for the Rust path.
    #[command(after_long_help = "PREREQUISITES (when implemented):
  - docker + docker compose on PATH
  - tonin.toml at --path

TODAY:
  Prints a stub message. For Rust services, use:
    cargo run -p <service>")]
    Run {
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum Lang {
    Rust,
    Python,
    Ts,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum ServiceType {
    Backend,
    Web,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq, Hash)]
pub enum ClientLang {
    /// Generate a Rust client crate (client-rust/).
    Rust,
    /// Generate a Python client package (client-python/).
    Python,
    /// Generate a TypeScript client package (client-ts/).
    Ts,
}

impl ClientLang {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClientLang::Rust => "rust",
            ClientLang::Python => "python",
            ClientLang::Ts => "ts",
        }
    }
    /// Which `Lang` is the implicit client for a given server language?
    /// Used to dedupe `--client-lang` against the server's own client.
    pub fn matches_server_lang(self, lang: Lang) -> bool {
        matches!(
            (self, lang),
            (ClientLang::Rust, Lang::Rust)
                | (ClientLang::Python, Lang::Python)
                | (ClientLang::Ts, Lang::Ts)
        )
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum WebMode {
    /// Vite + React, served as static files by nginx.
    Spa,
    /// Next.js Backend-for-Frontend. Server proxies/aggregates declared backends.
    Bff,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum StorageKind {
    /// AWS S3 or any S3-compatible (MinIO, R2, Wasabi).
    S3,
    /// Google Cloud Storage.
    Gcs,
    /// Azure Blob Storage.
    Azure,
    /// Local filesystem — useful for dev / single-node.
    Local,
}

impl StorageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            StorageKind::S3 => "s3",
            StorageKind::Gcs => "gcs",
            StorageKind::Azure => "azure",
            StorageKind::Local => "local",
        }
    }

    /// opendal Cargo feature name that backs this kind.
    pub fn opendal_feature(&self) -> &'static str {
        match self {
            StorageKind::S3 => "services-s3",
            StorageKind::Gcs => "services-gcs",
            StorageKind::Azure => "services-azblob",
            StorageKind::Local => "services-fs",
        }
    }
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Ts => "ts",
        }
    }
    pub fn default_type(&self) -> ServiceType {
        match self {
            Lang::Ts => ServiceType::Web,
            _ => ServiceType::Backend,
        }
    }
}

impl ServiceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceType::Backend => "backend",
            ServiceType::Web => "web",
        }
    }
}

impl WebMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            WebMode::Spa => "spa",
            WebMode::Bff => "bff",
        }
    }
}

pub fn run(cmd: ServiceCmd) -> Result<()> {
    match cmd {
        ServiceCmd::New {
            name,
            lang,
            r#type,
            web_mode,
            no_workspace,
            with_jobs,
            with_storage,
            client_langs,
        } => {
            let st = r#type.unwrap_or_else(|| lang.default_type());
            if st == ServiceType::Web && !matches!(lang, Lang::Ts) {
                bail!("--type web is only supported with --lang ts");
            }
            if web_mode.is_some() && !(st == ServiceType::Web && matches!(lang, Lang::Ts)) {
                bail!("--web-mode is only valid with --lang ts --type web");
            }
            if !with_jobs.is_empty() && !matches!(lang, Lang::Rust | Lang::Python) {
                bail!("--with-job is supported only for --lang rust|python");
            }
            if with_storage.is_some() && !matches!(lang, Lang::Rust | Lang::Python) {
                bail!("--with-storage is supported only for --lang rust|python");
            }
            // Default web mode is SPA.
            let wm = if st == ServiceType::Web {
                Some(web_mode.unwrap_or(WebMode::Spa))
            } else {
                None
            };

            // Dedupe --client-lang: skip any entry that matches the
            // server's own implicit client. Keep order, preserve first.
            let mut extras: Vec<ClientLang> = Vec::new();
            for cl in client_langs {
                if cl.matches_server_lang(lang) {
                    continue;
                }
                if !extras.contains(&cl) {
                    extras.push(cl);
                }
            }

            new::run(
                &name,
                lang,
                st,
                wm,
                no_workspace,
                &with_jobs,
                with_storage,
                &extras,
            )
        }
        ServiceCmd::Run { path } => {
            eprintln!("tonin service run: not yet implemented");
            eprintln!("path = {}", path.display());
            Ok(())
        }
    }
}
