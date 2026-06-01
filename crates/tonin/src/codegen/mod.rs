//! tonin-codegen
//!
//! Two responsibilities:
//!
//! 1. **Plan**: a typed deployment plan for one service, loaded from
//!    `tonin.toml`. The workspace loader walks all sibling services to
//!    compute the inverse `depends_on` graph (callers), so each plan knows
//!    both who it depends on and who depends on it. That graph is what
//!    network-policy generation consumes.
//!
//! 2. **Render**: turn a `Plan` into a set of YAML files using Tera.
//!    Templates live in `templates/k8s/` (mesh-agnostic) and
//!    `templates/k8s/mesh/<mesh>/` (mesh-specific). The bundled templates
//!    are embedded at compile time via `include_dir!` so the CLI works
//!    without the repo on disk.
//!
//! ## Codec
//!
//! Today every `.proto` is compiled with **prost** via `tonic-build`
//! (see `tonin-build::compile`). The `Buffa` variant + the
//! `[service].codec` field in `tonin.toml` reserve the surface for a
//! future `protoc-gen-micro` codegen plugin; in 0.x both variants
//! currently route through tonic-build.
#[allow(dead_code)] // codec field parsed but not yet acted on by the renderer
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Codec {
    /// prost via tonic-build. The only codegen path that runs today.
    #[default]
    Prost,
    /// Reserved for a future `protoc-gen-micro` codegen plugin.
    /// Currently routes through prost; setting this is a no-op.
    Buffa,
}

// These submodules were lifted from the old `tonin-codegen` crate. They
// expose a broad `pub` surface used internally by the CLI; the previous
// crate boundary made some items reachable externally that the CLI itself
// doesn't touch (yet). Silence the dead-code lint at the module level so
// the surface stays intact for future CLI commands without per-item attrs.
#[allow(dead_code)]
pub mod plan;
#[allow(dead_code)]
pub mod render;
#[allow(dead_code)]
pub mod stateful;

/// Standard docs tree every tonin service ships with.
///
/// Convention:
/// - `docs/README.md`        — explains the docs layout itself
/// - `docs/roadmap.md`       — done / active / future, one rolling list
/// - `docs/capabilities/`    — one .md per public capability the service offers
/// - `docs/plans/<name>/`    — one folder per active feature/project; each
///   holds `PRD.md` (product) + `TechSpec.md` (technical)
/// - `docs/plans/archive/`   — where completed plans move on done
/// - `AGENTS.md`             — at repo root, points coding agents at docs/
///
/// Returns Vec<(relative_path, contents)>. Caller writes them.
pub fn default_docs_tree(service_name: &str) -> Vec<(String, String)> {
    vec![
        ("AGENTS.md".to_string(), agents_md(service_name)),
        ("docs/README.md".to_string(), docs_readme(service_name)),
        ("docs/roadmap.md".to_string(), docs_roadmap(service_name)),
        (
            "docs/capabilities/.gitkeep".to_string(),
            gitkeep_capabilities(),
        ),
        ("docs/plans/.gitkeep".to_string(), gitkeep_plans()),
        ("docs/plans/archive/.gitkeep".to_string(), gitkeep_archive()),
    ]
}

fn agents_md(service_name: &str) -> String {
    format!(
        "# AGENTS.md — {name}

This file is the entry point for any coding agent working in this repo.

## Before you write code

1. Read `docs/README.md` for the docs convention.
2. Read `CLAUDE.md` for quick facts about this service (language, framework, config).
3. Read `docs/roadmap.md` to understand what's done, active, and planned.
4. Read `docs/capabilities/*.md` for what this service already does (no need to rediscover).

## When starting a new feature / project

Create `docs/plans/<feature-name>/` and seed it with two files:

- **`PRD.md`** — Product Requirements Document. Cover:
  - The user story this serves
  - The persona(s) involved
  - The concrete use case(s) end-to-end
  - Success criteria
  - Out of scope / explicit non-goals
- **`TechSpec.md`** — Technical Specification. Cover:
  - Architecture diagram (ASCII or mermaid, no external image hosting)
  - Data model changes (if any)
  - API surface — new RPCs in `proto/`
  - Storage shape (DB migrations, cache keys, etc.)
  - Dependencies on other services
  - Failure modes + retry / idempotency story
  - Observability — what spans, metrics, logs this adds

Both files exist before any code change. They evolve as the work
progresses. Reviewers expect them.

## When finishing a feature

The agent that closes out a feature is responsible for:

1. **Move the plan to archive.** `mv docs/plans/<feature> docs/plans/archive/<feature>`.
2. **Update `docs/capabilities/`.** Add a new `<capability>.md` if the
   feature added a new capability, OR edit an existing one if the
   feature extended one. The capability doc describes WHAT the service
   does, not HOW — link back to the archived plan for the HOW.
3. **Update `docs/roadmap.md`.** Move the entry from \"active\" to \"done\".

Skipping these steps means the next agent has to rediscover what the
service does. Don't skip.

## Anti-patterns

- Don't put implementation detail in `docs/capabilities/`. It rots.
  Link to the archived plan instead.
- Don't write PRDs as bug reports. They're the *intended* shape of the
  feature; bugs are tracked separately.
- Don't archive a plan without updating capabilities — the loss of
  information is silent.
",
        name = service_name,
    )
}

fn docs_readme(service_name: &str) -> String {
    format!(
        "# docs/

Living documentation for `{name}`. Read this before adding new docs.

## Layout

```
docs/
├── README.md              ← you are here
├── roadmap.md             ← rolling list: done / active / future
├── capabilities/          ← one .md per public capability (current state)
│   └── *.md
└── plans/
    ├── <feature>/         ← in-flight feature work
    │   ├── PRD.md         ← product: user story, persona, success criteria
    │   └── TechSpec.md    ← technical: architecture, data model, RPCs, ops
    └── archive/
        └── <done-feature>/  ← finished work, preserved for reference
```

## What each thing is for

### `capabilities/`

What the service does **today**. One file per coarse-grained capability
(e.g. `subscription.md`, `credit-ledger.md`, `invoicing.md`). Each file
answers: \"if I'm a peer service or consumer, what can I rely on this
service to do?\"

Capabilities describe **WHAT**, not HOW. The HOW lives in the archived
plan that introduced the capability — link to it.

### `plans/<feature>/`

Active feature work. Two files per plan:

- `PRD.md` — Product Requirements Document. The user story + persona +
  use case. Written first, refined as understanding grows. Reviewable
  by non-engineers.
- `TechSpec.md` — Technical specification. Architecture, data model,
  API changes, observability. Reviewable by engineers + ops.

Create the folder before any code. Both files exist before the first
commit on the feature branch.

### `plans/archive/<feature>/`

Where completed plans land. The agent that ships a feature **moves the
folder here** as part of the closing PR. The capability that resulted
is updated in `capabilities/`; the archived plan stays for historical
context.

### `roadmap.md`

Rolling timeline. Three sections: **Done** (with link to archived
plan), **Active** (with link to current plan folder), **Future**
(no folder yet; ideas with one-paragraph triggers).

## Lifecycle

```
idea          → roadmap.md \"Future\" section
                     │
                     ▼
feature kickoff → docs/plans/<feature>/{{PRD,TechSpec}}.md
                  + roadmap.md \"Active\"
                     │
                     ▼
work in progress → edit PRD/TechSpec as understanding sharpens;
                  capabilities/ stays UNCHANGED until done
                     │
                     ▼
feature ships    → mv docs/plans/<feature> docs/plans/archive/
                  + update or add docs/capabilities/<cap>.md
                  + move roadmap.md entry to \"Done\"
```

## See also

- `../AGENTS.md` — entry point for coding agents
- `../CLAUDE.md` — service quick facts (language, framework, dev commands)
",
        name = service_name,
    )
}

fn docs_roadmap(service_name: &str) -> String {
    format!(
        "# {name} — roadmap

Rolling history. Each entry has a one-line summary + link to the plan
(in `plans/` for active work, `plans/archive/` for done).

## Done

(empty — services start with no shipped features in their own roadmap.
The fact that the service was extracted from a monolith counts as
\"inherited capability\", documented under `capabilities/`, not here.)

## Active

(empty — add an entry when starting a new feature.)

Template:
- **<feature-name>** — one-line summary. → `plans/<feature-name>/`

## Future

Ideas / triggers without dedicated plans yet. Keep these short; promote
to `Active` (with a `plans/` folder) when the work actually starts.

Template:
- **<idea>** — what's the trigger to do this? what's the rough shape?

---

When updating this file, also touch `capabilities/` (if a capability
changed) and ensure the plan folder is in the right place
(`plans/<active>/` vs `plans/archive/<done>/`).
",
        name = service_name,
    )
}

fn gitkeep_capabilities() -> String {
    "# Add one .md per capability the service offers.\n\
# See ../README.md for the convention.\n"
        .to_string()
}

fn gitkeep_plans() -> String {
    "# Add one folder per active feature/project.\n\
# Each folder must contain PRD.md and TechSpec.md.\n\
# See ../README.md for the convention.\n"
        .to_string()
}

fn gitkeep_archive() -> String {
    "# Completed plans move here on feature close.\n\
# See ../../README.md for the convention.\n"
        .to_string()
}

/// Canonical `.gitignore` for a tonin service.
///
/// Modeled after `cargo new`'s default `.gitignore` (just `/target`),
/// expanded with reasonable defaults for the framework's ecosystem:
/// - Rust build artifacts
/// - Python `__pycache__` + `.venv` (for `--lang python` scaffolds)
/// - Node `node_modules` + `.next` + `dist` (for `--lang ts` scaffolds)
/// - Editor / OS noise
/// - Secrets (never commit)
/// - Claude Code per-user settings
///
/// **Intentionally NOT ignored:**
/// - `k8s/` — generated YAML is committed so PRs show its diff,
///   GitOps tools can sync from it, and `git blame` works.
/// - `Cargo.lock` — committed for binaries / workspaces (this repo is
///   both). The agnitiv convention also commits it.
/// - `proto/` — generated by `tonin proto extract` but committed so
///   reviewers can diff the wire contract.
pub fn default_rust_gitignore() -> &'static str {
    r#"# Rust build artifacts.
/target
**/*.rs.bk

# Python (when scaffolded with --lang python).
__pycache__/
*.py[cod]
.venv/
.python-version

# Node / TS (when scaffolded with --lang ts).
node_modules/
.next/
dist/

# IDE / OS noise.
.DS_Store
.idea/
.vscode/
*.swp
*.swo

# Local secrets — never commit. Replace placeholders in
# k8s/db-secret.yaml via `kubectl create secret` or ExternalSecret.
*.local.yaml
*.local.toml
.env
.env.*

# Claude Code per-user settings.
.claude/settings.local.json

# NOTE: k8s/, Cargo.lock, and proto/ are INTENTIONALLY committed.
# - k8s/    : PR reviewers + GitOps need to see/sync the manifests.
# - Cargo.lock : binaries + workspaces commit it; we are both.
# - proto/  : the wire contract belongs in git.
"#
}
