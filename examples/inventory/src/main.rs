// Large pod: holds an in-memory stock cache, so it asks for more RAM than peers.
use tonin::prelude::*;

#[tokio::main]
async fn main() -> tonin::Result<()> {
    Service::new("inventory").run().await
}
