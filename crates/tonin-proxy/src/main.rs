mod breaker;
mod cache;
mod coalesce;
mod config;
mod proxy;
mod retry;

use anyhow::Result;
use tracing::Level;
use tracing_subscriber::fmt;

#[tokio::main]
async fn main() -> Result<()> {
    let level = std::env::var("TONIN_PROXY_LOG")
        .ok()
        .and_then(|v| v.parse::<Level>().ok())
        .unwrap_or(Level::INFO);
    fmt().with_max_level(level).init();

    let cfg = config::Config::from_env()?;
    proxy::run(&cfg).await
}
