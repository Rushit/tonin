//! `tonin describe` — machine-readable manifest of the CLI surface.
//!
//! Walks the clap `Command` tree and emits every subcommand, every
//! argument, and the long-form help (including the prerequisite /
//! example blocks) as either a flat text dump or structured JSON.
//!
//! Coding agents use this to plan against the CLI without scraping
//! free-form `--help` output: parse the JSON once, then issue
//! invocations the same way you would call any documented API.
//!
//! ## JSON schema
//!
//! ```text
//! {
//!   "name": "tonin",
//!   "version": "0.1.0",
//!   "about": "...",
//!   "long_about": "...",
//!   "subcommands": [
//!     {
//!       "name": "k8s",
//!       "path": "tonin k8s",
//!       "about": "...",
//!       "long_about": "...",
//!       "after_long_help": "PREREQUISITES:\n  ...",
//!       "args": [
//!         { "name": "path", "long": "path", "default": ".",
//!           "required": false, "value_hint": "...", "help": "..." }
//!       ],
//!       "subcommands": [...]
//!     }
//!   ]
//! }
//! ```

use std::io::Write;

use anyhow::Result;
use clap::Command;
use serde::Serialize;

#[derive(clap::Args, Clone)]
pub struct DescribeArgs {
    /// Output format. `text` is human-skimmable; `json` is for tooling.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    pub format: Format,
    /// Limit to a single subcommand path, e.g. `tonin describe --only k8s.apply`.
    /// Drill into nested commands with `.` as the separator. Unset emits the
    /// full tree.
    #[arg(long)]
    pub only: Option<String>,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum Format {
    Text,
    Json,
}

#[derive(Serialize, Debug)]
struct Manifest<'a> {
    name: &'a str,
    version: Option<String>,
    about: Option<String>,
    long_about: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<ArgInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subcommands: Vec<NodeManifest>,
}

#[derive(Serialize, Debug)]
struct NodeManifest {
    name: String,
    path: String,
    about: Option<String>,
    long_about: Option<String>,
    after_long_help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<ArgInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subcommands: Vec<NodeManifest>,
}

#[derive(Serialize, Debug)]
struct ArgInfo {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    long: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    short: Option<char>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    possible_values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    required: bool,
    /// True if the flag can be passed multiple times (`--with-job foo --with-job bar`).
    repeatable: bool,
    /// False for boolean toggles (`--workspace`, `--dry-run`) — the flag
    /// itself is the value, no argument follows.
    takes_value: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    long_help: Option<String>,
}

pub fn run(args: DescribeArgs, root: &Command) -> Result<()> {
    let manifest = build_manifest(root);
    let filtered = match args.only.as_deref() {
        Some(path) => filter_manifest(&manifest, path)?,
        None => manifest,
    };
    let mut out = std::io::stdout().lock();
    match args.format {
        Format::Json => {
            serde_json::to_writer_pretty(&mut out, &filtered)?;
            writeln!(out)?;
        }
        Format::Text => {
            write_text(&mut out, &filtered)?;
        }
    }
    Ok(())
}

fn build_manifest<'a>(root: &'a Command) -> Manifest<'a> {
    Manifest {
        name: root.get_name(),
        version: root.get_version().map(|s| s.to_string()),
        about: root.get_about().map(|s| s.to_string()),
        long_about: root.get_long_about().map(|s| s.to_string()),
        args: collect_args(root),
        subcommands: root
            .get_subcommands()
            .filter(|sc| !sc.is_hide_set() && sc.get_name() != "help")
            .map(|sc| node(sc, root.get_name()))
            .collect(),
    }
}

fn node(cmd: &Command, parent_path: &str) -> NodeManifest {
    let path = format!("{parent_path} {}", cmd.get_name());
    NodeManifest {
        name: cmd.get_name().to_string(),
        path,
        about: cmd.get_about().map(|s| s.to_string()),
        long_about: cmd.get_long_about().map(|s| s.to_string()),
        after_long_help: cmd.get_after_long_help().map(|s| s.to_string()),
        args: collect_args(cmd),
        subcommands: cmd
            .get_subcommands()
            .filter(|sc| !sc.is_hide_set() && sc.get_name() != "help")
            .map(|sc| node(sc, &format!("{parent_path} {}", cmd.get_name())))
            .collect(),
    }
}

fn collect_args(cmd: &Command) -> Vec<ArgInfo> {
    cmd.get_arguments()
        .filter(|a| !a.is_hide_set())
        .map(|a| {
            // Boolean flags (`#[arg(long)] foo: bool`) are SetTrue/SetFalse
            // — they take no value at the CLI even though clap reports a
            // synthetic value_name + ["true","false"] possible-values
            // internally. Suppress those so agents see the actual
            // invocation syntax (just `--workspace`, no value).
            let is_bool_flag = matches!(
                a.get_action(),
                clap::ArgAction::SetTrue
                    | clap::ArgAction::SetFalse
                    | clap::ArgAction::Help
                    | clap::ArgAction::Version
            );
            let possible_values: Vec<String> = if is_bool_flag {
                Vec::new()
            } else {
                a.get_possible_values()
                    .iter()
                    .map(|pv| pv.get_name().to_string())
                    .collect()
            };
            let value_name = if is_bool_flag {
                None
            } else {
                a.get_value_names()
                    .and_then(|vn| vn.first().map(|v| v.to_string()))
            };
            let default = a
                .get_default_values()
                .iter()
                .next()
                .map(|v| v.to_string_lossy().to_string());
            ArgInfo {
                name: a.get_id().to_string(),
                long: a.get_long().map(|s| s.to_string()),
                short: a.get_short(),
                value_name,
                possible_values,
                default,
                required: a.is_required_set(),
                repeatable: matches!(
                    a.get_action(),
                    clap::ArgAction::Append | clap::ArgAction::Count
                ),
                takes_value: !is_bool_flag,
                help: a.get_help().map(|s| s.to_string()),
                long_help: a.get_long_help().map(|s| s.to_string()),
            }
        })
        .collect()
}

fn filter_manifest(manifest: &Manifest<'_>, path: &str) -> Result<Manifest<'static>> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut node_subs: &[NodeManifest] = &manifest.subcommands;
    let mut found: Option<&NodeManifest> = None;
    for part in &parts {
        let next = node_subs
            .iter()
            .find(|n| n.name == *part)
            .ok_or_else(|| anyhow::anyhow!("no such subcommand: '{part}' (in path '{path}')"))?;
        node_subs = &next.subcommands;
        found = Some(next);
    }
    let n = found.expect("path was non-empty so a node was found");
    Ok(Manifest {
        name: "tonin",
        version: manifest.version.clone(),
        about: Some(format!("filtered to: {}", n.path)),
        long_about: None,
        args: Vec::new(),
        subcommands: vec![clone_node(n)],
    })
}

fn clone_node(n: &NodeManifest) -> NodeManifest {
    NodeManifest {
        name: n.name.clone(),
        path: n.path.clone(),
        about: n.about.clone(),
        long_about: n.long_about.clone(),
        after_long_help: n.after_long_help.clone(),
        args: n
            .args
            .iter()
            .map(|a| ArgInfo {
                name: a.name.clone(),
                long: a.long.clone(),
                short: a.short,
                value_name: a.value_name.clone(),
                possible_values: a.possible_values.clone(),
                default: a.default.clone(),
                required: a.required,
                repeatable: a.repeatable,
                takes_value: a.takes_value,
                help: a.help.clone(),
                long_help: a.long_help.clone(),
            })
            .collect(),
        subcommands: n.subcommands.iter().map(clone_node).collect(),
    }
}

fn write_text(out: &mut impl Write, m: &Manifest<'_>) -> Result<()> {
    writeln!(out, "{} {}", m.name, m.version.as_deref().unwrap_or(""))?;
    if let Some(a) = &m.about {
        writeln!(out, "  {a}")?;
    }
    writeln!(out)?;
    for sc in &m.subcommands {
        write_node(out, sc, 0)?;
    }
    Ok(())
}

fn write_node(out: &mut impl Write, n: &NodeManifest, depth: usize) -> Result<()> {
    let indent = "  ".repeat(depth);
    writeln!(out, "{indent}{}", n.path)?;
    if let Some(a) = &n.about {
        writeln!(out, "{indent}  {a}")?;
    }
    for arg in &n.args {
        let mut spec = String::new();
        if let Some(l) = &arg.long {
            spec.push_str(&format!("--{l}"));
        } else {
            spec.push_str(&arg.name);
        }
        if let Some(vn) = &arg.value_name {
            spec.push_str(&format!(" <{vn}>"));
        }
        if !arg.possible_values.is_empty() {
            spec.push_str(&format!(" [{}]", arg.possible_values.join("|")));
        }
        if let Some(d) = &arg.default {
            spec.push_str(&format!(" (default: {d})"));
        }
        if arg.required {
            spec.push_str(" *required");
        }
        if arg.repeatable {
            spec.push_str(" *repeatable");
        }
        writeln!(out, "{indent}    {spec}")?;
        if let Some(h) = &arg.help {
            writeln!(out, "{indent}      {h}")?;
        }
    }
    if let Some(alh) = &n.after_long_help {
        // First line only in text mode — full block lives in JSON.
        let first = alh.lines().next().unwrap_or("");
        if !first.is_empty() {
            writeln!(out, "{indent}    (see --help: {first})")?;
        }
    }
    for sc in &n.subcommands {
        write_node(out, sc, depth + 1)?;
    }
    Ok(())
}
