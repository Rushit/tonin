# tonin-plugin

Minimal Plan API for `tonin` plugin authors.

Reads `tonin.toml` and resolves environment overlays without pulling in the
CLI dep tree (no clap, tera, include_dir, or codegen machinery). Intended as
the stable bridge between services and the tools that deploy them.

```toml
[dependencies]
tonin-plugin = "0.5"
```

## Core usage

```rust
use tonin_plugin::{Plan, select_env};

let env  = select_env(None);                      // reads $TONIN_ENV, defaults to "dev"
let plan = Plan::load_with_env(path, &env)?;
println!("deploying {} → {}", plan.name, plan.namespace);
```

## Version axes

tonin is split into three independently-versioned crates so each can evolve
at its own pace:

```
tonin-sdk          tonin-plugin          tonin (CLI)
(runtime)          (bridge)              (tool)
─────────          ────────────          ──────────
bumps on:          bumps on:             bumps on:
  Rust API           tonin.toml schema     new commands
  new capabilities   new Plan fields       new templates
  tokio/tonic major  new [section]         deploy options

pinned by:         pinned by:            pinned by:
  service devs       plugin authors        infra / devs
  (compile-time)     (plugin dev time)     (install once)
```

A service pinning `tonin-sdk = "0.6"` can be deployed by CLI `1.1` or
`1.2` — the CLI version is independent and irrelevant at runtime.

## Compatibility matrix

| tonin-sdk | tonin-plugin | tonin CLI | What changed |
|-----------|--------------|-----------|--------------|
| 0.5.x     | 0.5.x        | 0.5.x     | Initial split; `tonin-plugin` extracted from CLI, `tonin-sdk` renamed from `tonin-core` |
| 0.5.x     | 0.4.x        | 0.5.x     | `tonin-sdk` gains EventBus trait (no schema change) |
| 0.5.x     | 0.5.x        | 0.5.x     | `[eventbus]` section added to `tonin.toml` |
| 0.6.x     | 0.5.x        | 0.6.x     | SecretStore improvements (no schema change) |
| 0.6.x     | 0.6.x        | 0.6.x     | `[config]` section added to `tonin.toml` |
| 1.0.x     | 1.0.x        | 1.0.x     | Stable API guarantee begins |

Rules:
- `tonin-sdk` patch/minor bumps **never** require `tonin-plugin` or CLI changes.
- `tonin-plugin` bumps when `tonin.toml` schema changes (new section or field).
- CLI minor bumps freely; CLI major only when commands break (rare).

## CLI version advisory (`RECOMMENDED_CLI_MIN`)

`tonin-plugin` exports a `RECOMMENDED_CLI_MIN` constant:

```rust
pub const RECOMMENDED_CLI_MIN: &str = "0.4.2";
```

The `tonin` CLI reads this at `tonin k8s generate` / `tonin helm generate`
time and warns (never errors) when it is older. This catches the common case
of a service repo upgrading `tonin-plugin` while the developer's installed
CLI is still the previous version.

To bump: update `RECOMMENDED_CLI_MIN` in `src/plan.rs` in the same commit
that adds the new `tonin.toml` field.

## Writing a plugin

```rust
// In your plugin's main.rs:
use tonin_plugin::{Plan, select_env};

fn main() -> anyhow::Result<()> {
    // tonin sets TONIN_SERVICE_DIR before exec()-ing your binary
    let dir = std::env::var("TONIN_SERVICE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap());

    let env  = select_env(args.env.as_deref());
    let plan = Plan::load_with_env(&dir.join("tonin.toml"), &env)?;

    // Now use plan.name, plan.namespace, plan.database, etc.
    Ok(())
}
```

Name your binary `tonin-<name>` and tonin dispatches to it automatically:

```bash
cargo install tonin-myplugin
tonin myplugin [args...]        # tonin finds and exec()s tonin-myplugin
```

Optionally support `--tonin-describe` to show a description in
`tonin plugin list --verbose`:

```rust
if std::env::args().nth(1).as_deref() == Some("--tonin-describe") {
    println!("One-line description of what this plugin does");
    return Ok(());
}
```
