//! Zero-config OTLP tracing + W3C TraceContext propagation.
//!
//! `Service::new()` calls `init(service_name)` exactly once. Users don't
//! touch this module directly except to inject the current span's W3C
//! context into outbound calls via [`propagate::inject_current_context`]
//! (codegen will do this for them).
//!
//! Configuration is environment-driven (OTel conventions):
//!   * `OTEL_EXPORTER_OTLP_ENDPOINT` — collector URL (default in-cluster:
//!     `http://otel-collector.observability.svc.cluster.local:4317`)
//!   * `OTEL_SERVICE_NAME`           — overrides the name passed to `init`
//!   * `RUST_LOG` / `OTEL_LOG_LEVEL` — log filter
//!
//! Telemetry is **on by default**. Set `TONIN_TELEMETRY=off` to disable.

use std::sync::OnceLock;

use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{Resource, propagation::TraceContextPropagator, trace::SdkTracerProvider};
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub mod capability_metrics;
pub mod propagate;

// Ergonomic re-exports — service code reaches these from
// `tonin::telemetry::<name>` without having to spell out `propagate::`.
pub use propagate::{
    extract_context_from_map, inject_current_context, inject_current_context_http,
    inject_current_context_map,
};

static INITIALIZED: OnceLock<()> = OnceLock::new();
static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();

const DEFAULT_ENDPOINT: &str = "http://otel-collector.observability.svc.cluster.local:4317";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("otlp exporter init failed: {0}")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
}

/// Install the global tracing subscriber + OTLP exporter + W3C propagator.
/// Idempotent. Returns Ok immediately if `TONIN_TELEMETRY=off`.
pub fn init(service_name: &str) -> Result<(), Error> {
    if std::env::var("TONIN_TELEMETRY").as_deref() == Ok("off") {
        let _ = INITIALIZED.set(());
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .try_init();
        return Ok(());
    }
    if INITIALIZED.get().is_some() {
        return Ok(());
    }

    // W3C TraceContext propagator — what produces/consumes `traceparent`
    // headers on cross-service calls.
    global::set_text_map_propagator(TraceContextPropagator::new());

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
    let name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| service_name.to_string());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let resource = Resource::builder()
        .with_attributes(vec![KeyValue::new(SERVICE_NAME, name.clone())])
        .build();
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();
    let tracer = provider.tracer(name);
    global::set_tracer_provider(provider.clone());
    let _ = PROVIDER.set(provider);

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .try_init()
        .ok();

    let _ = INITIALIZED.set(());
    Ok(())
}

/// Flush + shut down the OTLP exporter. Call before process exit.
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get() {
        let _ = provider.shutdown();
    }
}
