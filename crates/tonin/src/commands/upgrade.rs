//! `tonin upgrade` — upgrade the tonin CLI and every installed plugin.
//!
//! Downloads the canonical `install.sh` and runs it with one `--plugin
//! <owner/repo>` flag per plugin discovered on `$PATH`. The repo comes from
//! each plugin's own `--tonin-meta`, so the CLI needs no hard-coded knowledge
//! of which plugins exist — a new `tonin-<name>` plugin is picked up
//! automatically as long as it reports its repo.
//!
//! The plan is printed and confirmed before anything changes (`--yes` skips
//! the prompt, `--check` previews only).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::commands::plugin::{self, PluginMeta};

/// Always fetch the latest install script from `main`.
const INSTALL_SH_URL: &str =
    "https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh";
/// Windows (PowerShell) equivalent of `install.sh`.
const INSTALL_PS1_URL: &str =
    "https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.ps1";
/// GitHub repo for the tonin CLI itself.
const TONIN_REPO: &str = "Rushit/tonin";

#[derive(clap::Args, Default)]
pub struct UpgradeArgs {
    /// Show the upgrade plan, then exit without changing anything.
    #[arg(long)]
    pub check: bool,

    /// Skip the confirmation prompt and upgrade immediately.
    #[arg(short, long)]
    pub yes: bool,

    /// Pin the tonin CLI to a specific version (default: latest release).
    #[arg(long, value_name = "vX.Y.Z")]
    pub version: Option<String>,

    /// Install directory (default: the directory of the running tonin binary).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Upgrade only the tonin CLI, leaving installed plugins untouched.
    #[arg(long)]
    pub self_only: bool,
}

/// One line of the upgrade plan.
struct PlanRow {
    label: String,
    current: String,
    target: String,
}

/// A discovered plugin we know how to upgrade (it reported a repo).
struct UpgradablePlugin {
    repo: String,
}

pub fn run(args: UpgradeArgs) -> Result<()> {
    let current_tonin = env!("CARGO_PKG_VERSION").to_string();

    // Where to install: explicit --dir, else next to the running binary.
    let dir = args.dir.clone().or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
    });

    // Build the plan. The tonin CLI is always part of it.
    let want_tonin = args
        .version
        .clone()
        .or_else(|| latest_tag(TONIN_REPO))
        .unwrap_or_else(|| "latest".to_string());
    let mut rows = vec![PlanRow {
        label: "tonin".to_string(),
        current: current_tonin,
        target: want_tonin,
    }];

    let mut upgradable: Vec<UpgradablePlugin> = Vec::new();
    let mut unmanaged: Vec<(String, String)> = Vec::new();

    if !args.self_only {
        for (name, path) in plugin::find_plugins() {
            let meta = plugin::query_meta(&path);
            let current = plugin_version(&meta, &path);
            match meta.as_ref().and_then(|m| m.repo.clone()) {
                Some(repo) => {
                    let target = latest_tag(&repo).unwrap_or_else(|| "latest".to_string());
                    rows.push(PlanRow {
                        label: format!("tonin-{name}"),
                        current,
                        target,
                    });
                    upgradable.push(UpgradablePlugin { repo });
                }
                // No repo metadata → we can't drive install.sh for it.
                None => unmanaged.push((name, current)),
            }
        }
    }

    print_plan(&rows, &unmanaged, dir.as_deref());

    if args.check {
        return Ok(());
    }

    // Nothing actually out of date? Don't bother shelling out.
    if rows.iter().all(|r| !r.is_upgrade()) {
        println!("\nEverything is up to date.");
        return Ok(());
    }

    if !args.yes && !confirm("Proceed with upgrade?") {
        eprintln!("aborted.");
        return Ok(());
    }

    run_install_script(dir.as_deref(), args.version.as_deref(), &upgradable)
}

impl PlanRow {
    /// `true` when the target is a concrete version newer than current.
    fn is_upgrade(&self) -> bool {
        !matches!(self.target.as_str(), "latest" | "?")
            && plugin::version_lt(&self.current, &self.target)
    }

    fn status(&self) -> &'static str {
        match self.target.as_str() {
            "latest" | "?" => "(latest unknown — will resolve)",
            _ if plugin::version_lt(&self.current, &self.target) => "⬆ upgrade",
            _ => "✓ up to date",
        }
    }
}

fn print_plan(rows: &[PlanRow], unmanaged: &[(String, String)], dir: Option<&Path>) {
    println!("Upgrade plan:\n");
    for r in rows {
        let target = r.target.trim_start_matches('v');
        println!(
            "  {:<18} {:<10} → {:<10}  {}",
            r.label,
            r.current,
            target,
            r.status()
        );
    }
    for (name, current) in unmanaged {
        let label = format!("tonin-{name}");
        println!(
            "  {label:<18} {current:<10}    (no repo metadata — upgrade with `cargo binstall`)"
        );
    }
    if let Some(d) = dir {
        println!("\nInstall directory: {}", d.display());
    }
}

/// Resolve a plugin's current version from its metadata, falling back to
/// `--version`, then `?`.
fn plugin_version(meta: &Option<PluginMeta>, path: &Path) -> String {
    meta.as_ref()
        .map(|m| m.version.clone())
        .or_else(|| plugin::query_version(path))
        .unwrap_or_else(|| "?".to_string())
}

/// Latest release tag for `owner/repo` via the GitHub API (best effort).
fn latest_tag(repo: &str) -> Option<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let output = Command::new("curl")
        .args([
            "-sSf",
            "-H",
            "User-Agent: tonin-cli",
            "-H",
            "Accept: application/vnd.github+json",
            &url,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    json.get("tag_name")?.as_str().map(str::to_string)
}

fn confirm(prompt: &str) -> bool {
    dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(true)
        .interact()
        .unwrap_or(false)
}

/// Download `install.sh` and run it with flags matching the plan.
fn run_install_script(
    dir: Option<&Path>,
    version: Option<&str>,
    plugins: &[UpgradablePlugin],
) -> Result<()> {
    // Same orchestration on every OS, different installer + shell: bash +
    // install.sh on Unix, PowerShell + install.ps1 on Windows. `cfg!(windows)`
    // (not `#[cfg]`) keeps both paths compiled and type-checked everywhere.
    if cfg!(windows) {
        run_via_powershell(dir, version, plugins)
    } else {
        run_via_bash(dir, version, plugins)
    }
}

fn run_via_bash(
    dir: Option<&Path>,
    version: Option<&str>,
    plugins: &[UpgradablePlugin],
) -> Result<()> {
    // Download to a temp file so the install dir path never has to survive
    // shell quoting through `bash -c`.
    let tmp = std::env::temp_dir().join("tonin-install.sh");
    let status = Command::new("curl")
        .args([
            "-sSfL",
            "--proto",
            "=https",
            "--tlsv1.2",
            INSTALL_SH_URL,
            "-o",
        ])
        .arg(&tmp)
        .status()
        .context("running curl to download install.sh (is curl installed?)")?;
    if !status.success() {
        bail!("failed to download install.sh from {INSTALL_SH_URL}");
    }

    let mut cmd = Command::new("bash");
    cmd.arg(&tmp);
    if let Some(d) = dir {
        cmd.arg("--dir").arg(d);
    }
    if let Some(v) = version {
        cmd.arg("--version").arg(v);
    }
    for p in plugins {
        cmd.arg("--plugin").arg(&p.repo);
    }

    let status = cmd.status().context("running install.sh")?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        bail!("install.sh exited with a failure");
    }
    println!("\n✓ Upgrade complete. Run `tonin --version` to verify.");
    Ok(())
}

fn run_via_powershell(
    dir: Option<&Path>,
    version: Option<&str>,
    plugins: &[UpgradablePlugin],
) -> Result<()> {
    let tmp = std::env::temp_dir().join("tonin-install.ps1");
    // Download via PowerShell itself (curl.exe isn't on every Windows).
    let download = format!(
        "Invoke-WebRequest -UseBasicParsing -Uri '{INSTALL_PS1_URL}' -OutFile '{}'",
        tmp.display()
    );
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &download,
        ])
        .status()
        .context("running powershell to download install.ps1 (is PowerShell installed?)")?;
    if !status.success() {
        bail!("failed to download install.ps1 from {INSTALL_PS1_URL}");
    }

    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&tmp);
    if let Some(d) = dir {
        cmd.arg("-Dir").arg(d);
    }
    if let Some(v) = version {
        cmd.arg("-Version").arg(v);
    }
    if !plugins.is_empty() {
        // install.ps1 takes a single comma-separated -Plugin and splits it.
        let joined = plugins
            .iter()
            .map(|p| p.repo.as_str())
            .collect::<Vec<_>>()
            .join(",");
        cmd.arg("-Plugin").arg(joined);
    }

    let status = cmd.status().context("running install.ps1")?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        bail!("install.ps1 exited with a failure");
    }
    println!("\n✓ Upgrade complete. Run `tonin --version` to verify.");
    Ok(())
}
