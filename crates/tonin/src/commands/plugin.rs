//! `tonin plugin` — list and inspect installed tonin plugins.

use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(clap::Subcommand)]
pub enum PluginCmd {
    /// List installed tonin plugins found on $PATH.
    ///
    /// Scans every directory in $PATH for executables named `tonin-<name>`.
    /// With `--verbose`, invokes each plugin with `--tonin-describe` to fetch
    /// a one-line description (plugins that don't support the flag are listed
    /// without a description).
    List {
        /// Also show a one-line description by invoking each plugin with
        /// `--tonin-describe`.
        #[arg(long)]
        verbose: bool,
    },
}

pub fn run(cmd: PluginCmd) -> Result<()> {
    match cmd {
        PluginCmd::List { verbose } => list(verbose),
    }
}

fn list(verbose: bool) -> Result<()> {
    let plugins = find_plugins();
    if plugins.is_empty() {
        println!("no tonin plugins installed");
        return Ok(());
    }
    for (name, path) in &plugins {
        if verbose {
            let desc = query_description(path).unwrap_or_default();
            println!("{name:<12}  {desc:<50}  {}", path.display());
        } else {
            println!("{name:<12}  {}", path.display());
        }
    }
    Ok(())
}

/// Scan every directory on `$PATH` for executables named `tonin-<name>`.
/// Returns entries sorted by plugin name; first occurrence on PATH wins.
pub fn find_plugins() -> Vec<(String, PathBuf)> {
    let Some(path_var) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut plugins: Vec<(String, PathBuf)> = Vec::new();

    for dir in std::env::split_paths(&path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if let Some(plugin_name) = name_str.strip_prefix("tonin-") {
                if plugin_name.is_empty() {
                    continue;
                }
                let path = entry.path();
                if is_executable(&path) && seen.insert(plugin_name.to_string()) {
                    plugins.push((plugin_name.to_string(), path));
                }
            }
        }
    }
    plugins.sort_by(|a, b| a.0.cmp(&b.0));
    plugins
}

/// Find the first `tonin-<name>` binary on `$PATH`, or `None`.
pub fn find_plugin(name: &str) -> Option<PathBuf> {
    let bin = format!("tonin-{name}");
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&bin);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn query_description(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--tonin-describe")
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        Some(text.lines().next()?.trim().to_string())
    } else {
        None
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
