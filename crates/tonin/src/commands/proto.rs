//! `tonin proto ...` — protobuf codegen.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum ProtoCmd {
    /// Codegen entry point for protobuf-defined services.
    ///
    /// **Stub today.** The scaffolded `build.rs` already compiles `.proto`
    /// files via `tonin-build` → `tonic-build` (prost) at `cargo build`
    /// time, which is what runs in every tonin service today. This
    /// command is the placeholder for a future `protoc-gen-micro` plugin
    /// that would invoke protoc directly with the tonin-flavoured
    /// output (server trait + client crate + MCP descriptors).
    #[command(after_long_help = "TODAY:
  Prints a stub message; the real codegen lives in build.rs and runs
  automatically on `cargo build`. To re-run codegen explicitly:

    cargo build --workspace          # rebuilds everything including .proto
    cargo build -p <service>         # rebuilds one service

PREREQUISITES (when implemented):
  - protoc on PATH
  - `protoc-gen-micro` plugin on PATH

SEE ALSO:
  docs/15-multi-language.md (cross-language codegen story)
  crates/tonin-build/README.md (the build.rs helper)")]
    Generate {
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}

pub fn run(cmd: ProtoCmd) -> Result<()> {
    match cmd {
        ProtoCmd::Generate { path } => {
            eprintln!("tonin proto generate: not yet implemented");
            eprintln!("path = {}", path.display());
            eprintln!("today: invoke tonic-build via build.rs (see examples/greeter/build.rs).");
            eprintln!("phase 2: this will shell out to protoc-gen-micro (buffa-based).");
            Ok(())
        }
    }
}
