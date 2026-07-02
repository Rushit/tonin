//! `tonin doctor` — check the local environment and installed plugins for
//! compatibility with this CLI.
//!
//! **Environment checks** run first: each required/optional tool is probed with
//! `env <tool> --version` (or equivalent) so the check works regardless of how
//! the tool was installed. Purely local (no network).
//!
//! **Plugin checks** follow: reads each plugin's `--tonin-meta` and compares
//! its declared minimum CLI version against the running `tonin`. When a plugin
//! needs a newer CLI it offers to run `tonin upgrade`.

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

/// A tool that `tonin doctor` probes for.
struct Tool {
    /// Binary name looked up via `env <name>`.
    name: &'static str,
    /// Human-readable label shown in output.
    label: &'static str,
    /// If false the tool is optional (warn, but don't count as a problem).
    required: bool,
    /// Extra args appended after `--version` when the default `--version`
    /// flag isn't the right probe (e.g. `python3 --version` works, but some
    /// tools use `version` subcommand instead).
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
        label: "just (task runner)",
        required: false,
        version_flag: "--version",
    },
    Tool {
        name: "gh",
        label: "GitHub CLI (gh)",
        required: false,
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

/// Probe a tool via `env <name> <version_flag>`.
///
/// Using `env` as the outer command means the lookup goes through the shell's
/// `PATH` even when the caller is not a shell (e.g. git hooks, CI runners).
fn probe_tool(tool: &Tool) -> Option<String> {
    let out = Command::new("env")
        .args([tool.name, tool.version_flag])
        .output()
        .ok()?;

    if out.status.success() {
        // Take only the first line of whatever `--version` prints.
        let raw = String::from_utf8_lossy(&out.stdout);
        Some(raw.lines().next().unwrap_or("").trim().to_string())
    } else {
        None
    }
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
