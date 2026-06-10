// Large pod: holds an in-memory stock cache, so it asks for more RAM than peers.
use tonin_sdk::prelude::*;

#[tokio::main]
async fn main() -> tonin_sdk::Result<()> {
    Service::new("inventory").run().await
}
