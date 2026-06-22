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

/// Machine-readable metadata a plugin reports via `--tonin-meta` (JSON).
///
/// Plugins that implement the flag let `tonin upgrade` / `tonin doctor`
/// discover their version, GitHub repo, and the minimum CLI they need —
/// no per-plugin knowledge baked into the CLI.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PluginMeta {
    /// Plugin short name, e.g. `helm` (matches the `tonin-<name>` binary).
    pub name: String,
    /// Plugin version (`X.Y.Z`).
    pub version: String,
    /// Minimum tonin CLI version this plugin needs, if it declares one.
    #[serde(default)]
    pub min_tonin: Option<String>,
    /// `owner/repo` on GitHub, used to fetch and install releases.
    #[serde(default)]
    pub repo: Option<String>,
}

/// Query a plugin's `--tonin-meta` JSON. `None` if the plugin predates the
/// protocol or emits anything unparseable.
pub fn query_meta(path: &Path) -> Option<PluginMeta> {
    let output = std::process::Command::new(path)
        .arg("--tonin-meta")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).ok()
}

/// Best-effort `<plugin> --version`, returning just the version token.
pub fn query_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // clap prints "<bin> X.Y.Z"; take the last whitespace token.
    text.split_whitespace().last().map(str::to_string)
}

/// `true` if semver `a` is strictly older than `b`. Unparseable versions
/// compare as "not older" so we never nag on garbage input.
pub fn version_lt(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => x < y,
        _ => false,
    }
}

/// Parse `vX.Y.Z` / `X.Y.Z[-pre]` into a comparable `(major, minor, patch)`.
/// Missing minor/patch default to 0; pre-release/build suffixes are ignored.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
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
