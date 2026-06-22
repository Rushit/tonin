//! `tonin doctor` — check installed plugins for compatibility with this CLI.
//!
//! Reads each plugin's `--tonin-meta` and compares its declared minimum CLI
//! version against the running `tonin`. Purely local (no network). When a
//! plugin needs a newer CLI it offers to run `tonin upgrade`.

use anyhow::Result;

use crate::commands::{plugin, upgrade};

#[derive(clap::Args)]
pub struct DoctorArgs {
    /// Only report problems; never offer to upgrade.
    #[arg(long)]
    pub no_fix: bool,
}

pub fn run(args: DoctorArgs) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("tonin {current}\n");

    let plugins = plugin::find_plugins();
    if plugins.is_empty() {
        println!("No plugins installed.");
        return Ok(());
    }

    let mut problems = 0;
    for (name, path) in &plugins {
        match plugin::query_meta(path) {
            Some(meta) => match &meta.min_tonin {
                Some(min) if plugin::version_lt(current, min) => {
                    problems += 1;
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

    if problems == 0 {
        println!("\nAll plugins are compatible.");
        return Ok(());
    }

    println!("\n{problems} plugin(s) need a newer tonin CLI.");
    if args.no_fix {
        println!("Run `tonin upgrade` to update.");
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
