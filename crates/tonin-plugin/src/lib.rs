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

pub mod build_backend;
pub mod config;
pub mod deploy_backend;
pub mod grpc_ui;
pub mod lock;
pub mod network;
pub mod observe_backend;
pub mod plan;
pub mod platform;
pub mod pod;
pub mod run_backend;
pub(crate) mod stateful;
pub mod workload;

// Config types and loaders
pub use config::ConfigLoader;

// Plan loading + schema types
pub use plan::{
    CURRENT_SCHEMA, ClientSpec, DEFAULT_DEPENDENCY_PORT, DependencyRef, Error, HealthSpec, McpMode,
    Mesh, MethodCacheSpec, Plan, RECOMMENDED_CLI_MIN, SUPPORTED_SCHEMAS, SecuritySection,
    ServiceKind, ServiceRef, WebMode, wave_groups,
};

// Network policy inputs ([callers], [egress])
pub use network::{Caller, CallerSource, CidrEgress, EgressSpec, FqdnEgress};

// Env resolution + resolved stateful types
pub use stateful::{
    CaMount, CacheEngine, CacheSpec, ConfigEngine, ConfigSpec, DatabaseEngine, DatabaseSpec,
    DatabaseTls, DatastoreEnv, DatastoreSource, EgressTarget, EmittedEnv, ExternalStore,
    MigrationMode, MigrationTool, MigrationsSpec, PG_CA_KEY, PG_CA_MOUNT_DIR, SecretEnvRef,
    SecretProvider, SecretsSpec, select_env,
};

// Workload shape ([deploy] kind, volumes, ports, probes, resources)
pub use workload::{
    AutoscaleSpec, PortSpec, ProbesSpec, ResourcesSpec, StartupProbeSpec, StrategySpec,
    VolumeSource, VolumeSpec, WorkloadKind, WorkloadSpec,
};

// Pod hygiene: pull secrets, placement, PDB, ServiceAccount, CronJobs
pub use pod::{
    CpuMemory, CronJobSpec, CronResources, IntOrPercent, PdbSpec, PlacementSpec, PodSettings,
    SaToken, ServiceAccountSpec, TokenSource, TokenVolume,
};

// Lock store types
pub use lock::{EnvLock, EnvironmentLock, GitLockStore, LockStore, ServiceLock};

// Build backend types
pub use build_backend::{
    BuildBackend, BuildDryRunBackend, BuildResult, BuildValues, DockerBackend,
};

// Deploy backend types
pub use deploy_backend::{
    DeployBackend, DeployEvent, DeployStatus, DeployValues, DryRunBackend, HelmBackend, ImageRef,
    NoopReporter, TelemetryReporter,
};

// Run backend types
pub use run_backend::DryRunBackend as RunDryRunBackend;
pub use run_backend::{CargoRunBackend, RunBackend, RunValues};

// Observe backend types
pub use observe_backend::{
    DockerComposeBackend, DryRunBackend as ObserveDryRunBackend, KubectlBackend, LogsValues,
    ObserveBackend, PortForwardValues,
};

// GrpcUI backend types
pub use grpc_ui::GrpcUiBackend;

// Platform orchestration types
pub use platform::{
    CliOrchestrator, DeployResult, DeploymentStatus, PlatformOrchestrator, ServiceStatus,
};
