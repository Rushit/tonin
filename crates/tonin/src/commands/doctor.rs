//! `tonin doctor` — check the local environment and installed plugins for
//! compatibility with this CLI.
//!
//! Checks whether required tools (`just`, `cargo`, `python3`) are installed
//! before checking plugin compatibility. Plugins that need a newer CLI offer
//! to run `tonin upgrade`.

use std::process::Command;

use anyhow::Result;

use crate::commands::{plugin, upgrade};

#[derive(clap::Args)]
pub struct DoctorArgs {
    /// Only report problems; never offer to upgrade.
    #[arg(long)]
    pub no_fix: bool,
}

// ---------------------------------------------------------------------------
// Environment tool checks
// ---------------------------------------------------------------------------

struct Tool {
    /// Binary name to look up on PATH.
    name: &'static str,
    /// Human-readable label shown in output.
    label: &'static str,
    /// If false the tool is optional (warn, don't count as a problem).
    required: bool,
    /// Flag used to retrieve the version string.
    version_flag: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "cargo",
        label: "Rust / Cargo",
        required: true,
        version_flag: "--version",
    },
    Tool {
        name: "python3",
        label: "Python 3 (pre-commit hook)",
        required: true,
        version_flag: "--version",
    },
    Tool {
        name: "just",
        label: "just (task runner — https://github.com/casey/just)",
        required: true,
        version_flag: "--version",
    },
    Tool {
        name: "protoc",
        label: "protoc (Protocol Buffers compiler)",
        required: false,
        version_flag: "--version",
    },
    Tool {
        name: "helm",
        label: "Helm",
        required: false,
        version_flag: "version",
    },
    Tool {
        name: "docker",
        label: "Docker",
        required: false,
        version_flag: "--version",
    },
];

/// Probe whether *tool* is available on `PATH`.
///
/// On Unix/macOS the lookup goes through `env <name>` so it works in
/// non-login shells (git hooks, CI). On Windows `env` is not a standard
/// command, so the tool is invoked directly — `Command` searches `PATH`
/// on all platforms.
fn probe_tool(tool: &Tool) -> Option<String> {
    #[cfg(unix)]
    let output = Command::new("env")
        .args([tool.name, tool.version_flag])
        .output()
        .ok()?;

    #[cfg(not(unix))]
    let output = Command::new(tool.name)
        .arg(tool.version_flag)
        .output()
        .ok()?;

    // Return the first non-empty line from stdout, then stderr (e.g. protoc).
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string);

    if output.status.success() { line } else { None }
}

fn check_environment() -> usize {
    println!("Environment checks:\n");
    let mut problems = 0;

    for tool in TOOLS {
        match probe_tool(tool) {
            Some(ver) => println!("  ✓ {} — {}", tool.label, ver),
            None if tool.required => {
                problems += 1;
                println!("  ✗ {} — not found (required)", tool.label);
            }
            None => {
                println!("  ⚠ {} — not found (optional)", tool.label);
            }
        }
    }

    println!();
    problems
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(args: DoctorArgs) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("tonin {current}\n");

    let env_problems = check_environment();

    // -----------------------------------------------------------------------
    // Plugin checks
    // -----------------------------------------------------------------------
    let plugins = plugin::find_plugins();
    if plugins.is_empty() {
        if env_problems == 0 {
            println!("No plugins installed. Environment looks healthy.");
        } else {
            println!(
                "{env_problems} required tool(s) missing. Install them and re-run `tonin doctor`."
            );
        }
        return Ok(());
    }

    println!("Plugin checks:\n");
    let mut plugin_problems = 0;
    for (name, path) in &plugins {
        match plugin::query_meta(path) {
            Some(meta) => match &meta.min_tonin {
                Some(min) if plugin::version_lt(current, min) => {
                    plugin_problems += 1;
                    println!(
                        "  ✗ tonin-{name} {} needs tonin >= {min} (you have {current})",
                        meta.version
                    );
                }
                Some(min) => {
                    println!("  ✓ tonin-{name} {} (needs tonin >= {min})", meta.version);
                }
                None => println!("  ✓ tonin-{name} {}", meta.version),
            },
            None => {
                let ver = plugin::query_version(path).unwrap_or_else(|| "?".to_string());
                println!("  ? tonin-{name} {ver} (no compatibility metadata)");
            }
        }
    }

    let total_problems = env_problems + plugin_problems;
    if total_problems == 0 {
        println!("\nAll checks passed.");
        return Ok(());
    }

    if plugin_problems > 0 {
        println!("\n{plugin_problems} plugin(s) need a newer tonin CLI.");
    }
    if env_problems > 0 {
        println!("{env_problems} required tool(s) are missing.");
    }

    if args.no_fix || plugin_problems == 0 {
        println!("Run `tonin upgrade` to update the CLI if needed.");
        return Ok(());
    }

    let proceed = dialoguer::Confirm::new()
        .with_prompt("Run `tonin upgrade` now?")
        .default(true)
        .interact()
        .unwrap_or(false);
    if proceed {
        upgrade::run(upgrade::UpgradeArgs::default())
    } else {
        println!("Run `tonin upgrade` when ready.");
        Ok(())
    }
}
