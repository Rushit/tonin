//! `build.rs` helper that wraps [`tonic-build`] with tonin conventions.
//!
//! Call [`compile`] from a service crate's `build.rs` with the protos to
//! compile and the include paths to resolve `import` statements against.
//! Both gRPC client and server stubs are generated; the output lands in
//! `OUT_DIR` and is pulled in with `tonic::include_proto!("<package>")`.
//!
//! # Example
//!
//! ```no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Skip codegen if protoc is unavailable so the workspace still
//!     // `cargo check`s without system protoc installed.
//!     // Set TONIN_SKIP_PROTOC=1 to skip.
//!     if std::env::var("TONIN_SKIP_PROTOC").is_ok() {
//!         return Ok(());
//!     }
//!     tonin_build::compile(&["proto/greeter.proto"], &["proto"])
//! }
//! ```
//!
//! # Cargo.toml
//!
//! ```toml
//! [build-dependencies]
//! tonin-build = "0.1"
//! ```
//!
//! Today every call to [`compile`] routes through `tonic-build` (prost).
//! `TONIN_CODEC=buffa` is reserved for a future `protoc-gen-micro` codegen
//! plugin; setting it today logs a notice to stderr and falls back to
//! prost so scaffolds keep working unchanged. `TONIN_CODEC=prost` (or
//! unset) is the explicit / default path.
//!
//! # Sample app
//!
//! <https://github.com/Rushit/tonin/tree/main/examples/greeter/blob/main/build.rs>
//!
//! # Sibling crates
//!
//! <https://docs.rs/tonin>
//!
//! [`tonic-build`]: https://crates.io/crates/tonic-build

pub enum Codec {
    Buffa,
    Prost,
}

impl Codec {
    fn from_env() -> Self {
        match std::env::var("TONIN_CODEC").as_deref() {
            Ok("prost") => Codec::Prost,
            _ => Codec::Buffa, // default
        }
    }
}

pub fn compile(protos: &[&str], includes: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    match Codec::from_env() {
        Codec::Buffa => {
            // TODO: invoke `protoc --plugin=protoc-gen-micro ...` once
            // the codegen plugin is published. Falling back to tonic-build
            // for now so the scaffold compiles end-to-end.
            eprintln!(
                "tonin-build: codec=buffa requested; falling back to prost (codegen plugin not yet wired)"
            );
            tonic_build::configure().compile_protos(protos, includes)?;
        }
        Codec::Prost => {
            tonic_build::configure().compile_protos(protos, includes)?;
        }
    }
    Ok(())
}
