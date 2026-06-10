// Minimal example: bind a service and run it.
// Once protoc is wired in, the user impls a generated trait and passes it via `.handler(...)`.
use tonin_sdk::prelude::*;

#[tokio::main]
async fn main() -> tonin_sdk::Result<()> {
    // Telemetry (OTLP tracing + log subscriber) is installed by Service::new.
    Service::new("greeter").run().await
}
