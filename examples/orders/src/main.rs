// Medium pod. Orders calls inventory via mesh DNS — no client config needed,
// Linkerd handles mTLS + retries.
use tonin_sdk::prelude::*;

#[tokio::main]
async fn main() -> tonin_sdk::Result<()> {
    let svc = Service::new("orders"); // installs telemetry
    let inventory_url = tonin_sdk::discovery::service_url("inventory", "shop");
    tracing::info!(%inventory_url, "orders will call inventory at this URL");
    svc.run().await
}
