fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("TONIN_SKIP_PROTOC").is_ok() {
        return Ok(());
    }
    tonin_build::compile(&["proto/orders.proto"], &["proto"])
}
