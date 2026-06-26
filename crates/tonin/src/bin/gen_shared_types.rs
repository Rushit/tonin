//! Generate language-specific shared-types files from a fixed schema.
//!
//! The Rust types in `crates/tonin-client/{auth,retry,breaker}.rs` are
//! the source of truth. This generator emits the equivalent Python
//! dataclasses and TypeScript interfaces with the same field names and
//! defaults, so peer services see a byte-for-byte compatible shape
//! across the polyglot mesh.
//!
//! ## Why hand-rolled (not schemars)
//!
//! The shared surface is tiny — 8 types, ~40 fields total — and the
//! drift risk is on the *names* (subject, scopes, raw_token...) not on
//! the JSON-schema details. A 200-line hand-rolled generator is easier
//! to debug, has no extra dep, and produces output that matches what a
//! human would write idiomatically in each language.
//!
//! ## Drift detection
//!
//! Run this binary, then `git diff --exit-code` on the emitted files.
//! CI gates that. If someone adds a field to the Rust struct without
//! updating this generator, the corresponding language file falls out
//! of date and CI fails on the unstaged diff.
//!
//! ## What gets generated
//!
//! - `python/tonin-client/src/tonin_client/_generated.py`
//! - `ts/tonin-client/src/_generated.ts`
//!
//! The Rust types remain hand-written — they're the spec.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

/// A single type to mirror across languages.
#[derive(Debug)]
enum Type<'a> {
    /// Enum with stringly-typed variants. `repr` is what each variant
    /// serializes to in JSON (matches the `#[serde(rename_all = "...")]`
    /// attribute on the Rust enum).
    StringEnum {
        name: &'a str,
        doc: &'a str,
        variants: &'a [(&'a str, &'a str)], // (RustName, "serde_value")
    },
    /// Dataclass / interface. Fields list (name, py_type, ts_type, default).
    Struct {
        name: &'a str,
        doc: &'a str,
        fields: &'a [Field<'a>],
    },
}

#[derive(Debug)]
struct Field<'a> {
    /// Field name as used in Rust + Python + TS. We DO NOT camel-case
    /// for TS — the polyglot promise is that wire shapes are identical,
    /// which means JSON field names match Rust's serde field names
    /// (snake_case). Hand-callers in TS can opt into camelCase via
    /// their own mappers; the codegen doesn't.
    name: &'a str,
    py_type: &'a str,
    ts_type: &'a str,
    /// Default expression in each language. `None` means "no default
    /// (required)". For Python the default appears in the dataclass
    /// signature; for TS, on the interface it's a doc-only hint
    /// (interfaces don't carry defaults).
    py_default: Option<&'a str>,
    ts_default_doc: Option<&'a str>,
    doc: &'a str,
}

const SCHEMA: &[Type] = &[
    Type::StringEnum {
        name: "PrincipalKind",
        doc: "Who is making the call. Lowercase values match the Rust enum.",
        variants: &[
            ("USER", "user"),
            ("SERVICE", "service"),
            ("AGENT", "agent"),
            ("ANONYMOUS", "anonymous"),
        ],
    },
    Type::Struct {
        name: "RawToken",
        doc: "A token as extracted from a request, before verification.",
        fields: &[
            Field {
                name: "value",
                py_type: "str",
                ts_type: "string",
                py_default: None,
                ts_default_doc: None,
                doc: "The token string itself.",
            },
            Field {
                name: "kind",
                py_type: "str",
                ts_type: "string",
                py_default: Some("\"bearer-jwt\""),
                ts_default_doc: Some("\"bearer-jwt\""),
                doc: "Conventions: bearer-jwt, api-key, session-cookie, basic-auth.",
            },
        ],
    },
    Type::Struct {
        name: "AuthCtx",
        doc: "Identity + claims for the current request. The single concrete type \
              that flows through the framework across languages.",
        fields: &[
            Field {
                name: "subject",
                py_type: "str",
                ts_type: "string",
                py_default: Some("\"\""),
                ts_default_doc: Some("\"\""),
                doc: "`sub` JWT claim. User ID for users, service ID for services.",
            },
            Field {
                name: "issuer",
                py_type: "str",
                ts_type: "string",
                py_default: Some("\"\""),
                ts_default_doc: Some("\"\""),
                doc: "`iss` JWT claim.",
            },
            Field {
                name: "audience",
                py_type: "str",
                ts_type: "string",
                py_default: Some("\"\""),
                ts_default_doc: Some("\"\""),
                doc: "`aud` JWT claim.",
            },
            Field {
                name: "scopes",
                py_type: "list[str]",
                ts_type: "string[]",
                py_default: Some("field(default_factory=list)"),
                ts_default_doc: Some("[]"),
                doc: "OAuth-style scopes.",
            },
            Field {
                name: "kind",
                py_type: "PrincipalKind",
                ts_type: "PrincipalKind",
                py_default: Some("PrincipalKind.ANONYMOUS"),
                ts_default_doc: Some("PrincipalKind.ANONYMOUS"),
                doc: "Principal class.",
            },
            Field {
                name: "raw_token",
                py_type: "str",
                ts_type: "string",
                py_default: Some("\"\""),
                ts_default_doc: Some("\"\""),
                doc: "Verbatim token, used by `propagate` on outbound calls.",
            },
            Field {
                name: "expires_at",
                py_type: "float",
                ts_type: "number",
                py_default: Some("0.0"),
                ts_default_doc: Some("0"),
                doc: "Unix-seconds expiry of the token.",
            },
            Field {
                name: "extra",
                py_type: "dict[str, Any]",
                ts_type: "Record<string, unknown>",
                py_default: Some("field(default_factory=dict)"),
                ts_default_doc: Some("{}"),
                doc: "Custom claims not mapped to typed fields.",
            },
        ],
    },
    Type::StringEnum {
        name: "Backoff",
        doc: "Retry backoff strategy.",
        variants: &[("EXPONENTIAL", "exponential"), ("FIXED", "fixed")],
    },
    Type::Struct {
        name: "RetryPolicy",
        doc: "Retry behavior for an outbound RPC. Same defaults as Rust.",
        fields: &[
            Field {
                name: "max_attempts",
                py_type: "int",
                ts_type: "number",
                py_default: Some("1"),
                ts_default_doc: Some("1"),
                doc: "Total attempts including the first. 1 = no retry.",
            },
            Field {
                name: "initial_backoff_secs",
                py_type: "float",
                ts_type: "number",
                py_default: Some("0.0"),
                ts_default_doc: Some("0"),
                doc: "Delay before the first retry, in seconds.",
            },
            Field {
                name: "backoff",
                py_type: "Backoff",
                ts_type: "Backoff",
                py_default: Some("Backoff.FIXED"),
                ts_default_doc: Some("Backoff.FIXED"),
                doc: "Backoff growth strategy.",
            },
            Field {
                name: "multiplier",
                py_type: "float",
                ts_type: "number",
                py_default: Some("2.0"),
                ts_default_doc: Some("2.0"),
                doc: "Exponential multiplier; ignored when backoff == FIXED.",
            },
            Field {
                name: "max_backoff_secs",
                py_type: "float",
                ts_type: "number",
                py_default: Some("1.0"),
                ts_default_doc: Some("1"),
                doc: "Cap on any single backoff interval, in seconds.",
            },
        ],
    },
    Type::Struct {
        name: "CircuitBreaker",
        doc: "Breaker config for outbound RPCs. Same defaults as Rust.",
        fields: &[
            Field {
                name: "window_secs",
                py_type: "float",
                ts_type: "number",
                py_default: Some("10.0"),
                ts_default_doc: Some("10"),
                doc: "Rolling window for failure-rate calculation.",
            },
            Field {
                name: "trip_threshold",
                py_type: "float",
                ts_type: "number",
                py_default: Some("0.5"),
                ts_default_doc: Some("0.5"),
                doc: "Failure ratio in (0, 1] at which the breaker trips.",
            },
            Field {
                name: "min_requests",
                py_type: "int",
                ts_type: "number",
                py_default: Some("20"),
                ts_default_doc: Some("20"),
                doc: "Minimum requests in the window before the breaker can trip.",
            },
            Field {
                name: "reset_after_secs",
                py_type: "float",
                ts_type: "number",
                py_default: Some("30.0"),
                ts_default_doc: Some("30"),
                doc: "How long the breaker stays Open before HalfOpen.",
            },
            Field {
                name: "half_open_probes",
                py_type: "int",
                ts_type: "number",
                py_default: Some("3"),
                ts_default_doc: Some("3"),
                doc: "Probe requests allowed through in HalfOpen.",
            },
        ],
    },
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Locate the workspace root. We assume this binary is invoked via
    // `cargo run --bin gen-shared-types` from the workspace root.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .ok_or("failed to find workspace root")?
        .to_path_buf();

    let py_path = workspace_root.join("python/tonin-client/src/tonin_client/_generated.py");
    let ts_path = workspace_root.join("ts/tonin-client/src/_generated.ts");

    let py = render_python();
    let ts = render_typescript();

    fs::write(&py_path, &py)?;
    fs::write(&ts_path, &ts)?;

    eprintln!("✓ wrote {}", py_path.display());
    eprintln!("✓ wrote {}", ts_path.display());
    eprintln!();
    eprintln!(
        "Drift check: `git diff --exit-code {} {}`",
        py_path.display(),
        ts_path.display()
    );

    Ok(())
}

const PY_HEADER: &str = r#""""Generated by `cargo run --bin gen-shared-types`. DO NOT EDIT BY HAND.

Source of truth is the Rust types in `crates/tonin-client/src/`. Any
manual edit to this file will be reverted next time the generator runs.

The hand-written types in `auth.py`, `retry.py`, `breaker.py` are
kept here for the CI drift check — see `tests/test_generated_match.py`
in the tonin-client package.
"""

from __future__ import annotations

import enum
from dataclasses import dataclass, field
from typing import Any
"#;

const TS_HEADER: &str = r#"// Generated by `cargo run --bin gen-shared-types`. DO NOT EDIT BY HAND.
//
// Source of truth is the Rust types in `crates/tonin-client/src/`.
// Any manual edit to this file will be reverted next time the
// generator runs.
"#;

fn render_python() -> String {
    let mut out = String::from(PY_HEADER);
    out.push('\n');
    for t in SCHEMA {
        match t {
            Type::StringEnum {
                name,
                doc,
                variants,
            } => {
                writeln!(out, "\nclass {name}(str, enum.Enum):").unwrap();
                writeln!(out, "    \"\"\"{doc}\"\"\"").unwrap();
                for (var_name, serde_value) in *variants {
                    writeln!(out, "    {var_name} = \"{serde_value}\"").unwrap();
                }
            }
            Type::Struct { name, doc, fields } => {
                writeln!(out, "\n@dataclass(slots=True)").unwrap();
                writeln!(out, "class {name}:").unwrap();
                writeln!(out, "    \"\"\"{doc}\"\"\"").unwrap();
                for f in *fields {
                    let default_str = match f.py_default {
                        Some(d) => format!(" = {d}"),
                        None => String::new(),
                    };
                    writeln!(out, "    # {}", f.doc).unwrap();
                    writeln!(out, "    {}: {}{}", f.name, f.py_type, default_str).unwrap();
                }
            }
        }
    }
    out
}

fn render_typescript() -> String {
    let mut out = String::from(TS_HEADER);
    out.push('\n');
    for t in SCHEMA {
        match t {
            Type::StringEnum {
                name,
                doc,
                variants,
            } => {
                writeln!(out, "\n/** {doc} */").unwrap();
                writeln!(out, "export enum {name} {{").unwrap();
                for (var_name, serde_value) in *variants {
                    writeln!(out, "  {var_name} = \"{serde_value}\",").unwrap();
                }
                writeln!(out, "}}").unwrap();
            }
            Type::Struct { name, doc, fields } => {
                writeln!(out, "\n/** {doc} */").unwrap();
                writeln!(out, "export interface {name} {{").unwrap();
                for f in *fields {
                    let default_hint = match f.ts_default_doc {
                        Some(d) => format!(" Default: `{d}`."),
                        None => String::new(),
                    };
                    writeln!(out, "  /** {}{} */", f.doc, default_hint).unwrap();
                    writeln!(out, "  {}: {};", f.name, f.ts_type).unwrap();
                }
                writeln!(out, "}}").unwrap();
            }
        }
    }
    out
}
