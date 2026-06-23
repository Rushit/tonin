//! Minimal Plan API for tonin plugin authors.
//!
//! Reads `tonin.toml` and resolves env overlays without pulling in the CLI
//! dep tree (no clap, tera, include_dir, or codegen machinery).
//!
//! # Usage in a plugin's `Cargo.toml`
//!
//! ```toml
//! [dependencies]
//! tonin-plugin = { version = "0.4", default-features = false }
//! ```
//!
//! # Core pattern
//!
//! ```no_run
//! use std::path::Path;
//! use tonin_plugin::{Plan, select_env};
//!
//! let env  = select_env(None);
//! let plan = Plan::load_with_env(Path::new("tonin.toml"), &env)?;
//! println!("deploying {} to {}", plan.name, plan.namespace);
//! # Ok::<(), tonin_plugin::Error>(())
//! ```

pub mod plan;
pub(crate) mod stateful;

// Plan loading + schema types
pub use plan::{
    CURRENT_SCHEMA, ClientSpec, Error, HealthSpec, Mesh, MethodCacheSpec, Plan,
    RECOMMENDED_CLI_MIN, SUPPORTED_SCHEMAS, SecuritySection, ServiceKind, ServiceRef, WebMode,
};

// Env resolution + resolved stateful types
pub use stateful::{
    CacheEngine, CacheSpec, ConfigEngine, ConfigSpec, DatabaseEngine, DatabaseSpec, EmittedEnv,
    ExternalStore, MigrationRunOn, MigrationTool, MigrationsSpec, SecretProvider, SecretsSpec,
    select_env,
};
