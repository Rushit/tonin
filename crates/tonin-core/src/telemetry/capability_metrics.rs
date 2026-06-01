//! OTel metric handles + slow-op thresholds for capability decorators.
//!
//! `tonin-core::Instrumented<T>` calls into this module on every cache,
//! database, and event-bus method call. Metric handles are lazily
//! initialised through the OTel global meter — if no MeterProvider has
//! been installed (Phase 3 ships without one), the global meter is a
//! no-op and `record_*` becomes a cheap no-op. Phase 5 will install a
//! real MeterProvider; nothing here changes.

use std::{sync::OnceLock, time::Duration};

use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Histogram},
};

#[derive(Clone, Debug)]
pub struct SlowThresholds {
    pub db_query: Duration,
    pub cache_op: Duration,
    pub messaging_publish: Duration,
}

impl Default for SlowThresholds {
    fn default() -> Self {
        Self {
            db_query: Duration::from_millis(200),
            cache_op: Duration::from_millis(20),
            messaging_publish: Duration::from_millis(50),
        }
    }
}

static CACHE_HIST: OnceLock<Histogram<f64>> = OnceLock::new();
static CACHE_COUNT: OnceLock<Counter<u64>> = OnceLock::new();
static DB_HIST: OnceLock<Histogram<f64>> = OnceLock::new();
static DB_COUNT: OnceLock<Counter<u64>> = OnceLock::new();
static MSG_PUB_HIST: OnceLock<Histogram<f64>> = OnceLock::new();
static MSG_PUB_COUNT: OnceLock<Counter<u64>> = OnceLock::new();

fn cache_hist() -> &'static Histogram<f64> {
    CACHE_HIST.get_or_init(|| {
        global::meter("tonin.cache")
            .f64_histogram("cache_op_duration_ms")
            .with_unit("ms")
            .build()
    })
}

fn cache_count() -> &'static Counter<u64> {
    CACHE_COUNT.get_or_init(|| {
        global::meter("tonin.cache")
            .u64_counter("cache_op_total")
            .build()
    })
}

fn db_hist() -> &'static Histogram<f64> {
    DB_HIST.get_or_init(|| {
        global::meter("tonin.db")
            .f64_histogram("db_query_duration_ms")
            .with_unit("ms")
            .build()
    })
}

fn db_count() -> &'static Counter<u64> {
    DB_COUNT.get_or_init(|| {
        global::meter("tonin.db")
            .u64_counter("db_query_total")
            .build()
    })
}

fn msg_pub_hist() -> &'static Histogram<f64> {
    MSG_PUB_HIST.get_or_init(|| {
        global::meter("tonin.messaging")
            .f64_histogram("messaging_publish_duration_ms")
            .with_unit("ms")
            .build()
    })
}

fn msg_pub_count() -> &'static Counter<u64> {
    MSG_PUB_COUNT.get_or_init(|| {
        global::meter("tonin.messaging")
            .u64_counter("messaging_publish_total")
            .build()
    })
}

pub fn record_cache_op(system: &str, op: &str, outcome: &str, elapsed: Duration) {
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    cache_hist().record(
        elapsed_ms,
        &[
            KeyValue::new("cache_system", system.to_string()),
            KeyValue::new("op", op.to_string()),
        ],
    );
    cache_count().add(
        1,
        &[
            KeyValue::new("cache_system", system.to_string()),
            KeyValue::new("op", op.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ],
    );
}

pub fn record_db_query(system: &str, operation: &str, outcome: &str, elapsed: Duration) {
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    db_hist().record(
        elapsed_ms,
        &[
            KeyValue::new("db_system", system.to_string()),
            KeyValue::new("db_operation", operation.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ],
    );
    db_count().add(
        1,
        &[
            KeyValue::new("db_system", system.to_string()),
            KeyValue::new("db_operation", operation.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ],
    );
}

pub fn record_messaging_publish(system: &str, destination: &str, outcome: &str, elapsed: Duration) {
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    msg_pub_hist().record(
        elapsed_ms,
        &[
            KeyValue::new("messaging_system", system.to_string()),
            KeyValue::new("destination", destination.to_string()),
        ],
    );
    msg_pub_count().add(
        1,
        &[
            KeyValue::new("messaging_system", system.to_string()),
            KeyValue::new("destination", destination.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ],
    );
}
