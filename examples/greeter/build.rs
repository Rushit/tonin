fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Skip codegen if protoc is unavailable so the workspace still `cargo check`s
    // without system protoc installed. Set TONIN_SKIP_PROTOC=1 to skip.
    if std::env::var("TONIN_SKIP_PROTOC").is_ok() {
        return Ok(());
    }
    tonin_build::compile(&["proto/greeter.proto"], &["proto"])
}
