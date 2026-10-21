pub mod affected;
pub mod build;
pub mod deploy;
pub mod describe;
pub mod doctor;
pub mod grpc_ui;
pub mod helm;
pub mod new;
pub mod observe;
pub mod platform;
pub mod plugin;
pub mod proto;
pub mod release;
pub mod run;
pub mod run_tests;
pub mod service;
pub mod status;
pub mod upgrade;

/// `--workspace` if given, else the current directory (an error, not a
/// panic, when the current directory is gone or unreadable).
pub(crate) fn workspace_or_cwd(
    workspace: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    match workspace {
        Some(dir) => Ok(dir),
        None => {
            use anyhow::Context as _;
            std::env::current_dir().context("cannot determine the current directory")
        }
    }
}
