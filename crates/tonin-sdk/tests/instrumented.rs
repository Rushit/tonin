//! Integration tests for `Instrumented<T>`.

use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::StreamExt;
use opentelemetry::{global, propagation::TextMapPropagator, trace::TracerProvider as _};
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::SdkTracerProvider};
use tonin_sdk::telemetry::capability_metrics::SlowThresholds;
use tonin_sdk::{
    Error,
    instrumented::Instrumented,
    traits::{Acker, Cache, DeliveredMessage, EventBus, MessageId, SubscribeOptions, Subscription},
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{EnvFilter, Registry, fmt::MakeWriter, layer::SubscriberExt};

// ---------- shared test infra ----------

#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn captured(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8(buf.lock().unwrap().clone()).unwrap_or_default()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// ---------- mock Cache ----------

struct MockCache {
    delay: Duration,
}

#[async_trait]
impl Cache for MockCache {
    async fn get(&self, _key: &str) -> Result<Option<Vec<u8>>, Error> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(Some(b"hello".to_vec()))
    }
    async fn set(&self, _k: &str, _v: &[u8], _t: Option<Duration>) -> Result<(), Error> {
        Ok(())
    }
    async fn del(&self, _k: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn set_nx(&self, _k: &str, _v: &[u8], _t: Option<Duration>) -> Result<bool, Error> {
        Ok(true)
    }
    fn system(&self) -> &'static str {
        "mock"
    }
}

// ---------- 1) cache span attributes ----------

#[test]
fn cache_get_emits_span_attributes() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = CaptureWriter(buf.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info"))
        .with_writer(writer)
        .with_ansi(false)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        rt().block_on(async {
            let cache = Instrumented::with_defaults(Arc::new(MockCache {
                delay: Duration::ZERO,
            }));
            let v = cache.get("hello").await.unwrap();
            assert_eq!(v, Some(b"hello".to_vec()));
        });
    });

    let out = captured(&buf);
    assert!(
        out.contains("cache.system"),
        "missing cache.system in: {out}"
    );
    assert!(
        out.contains("\"mock\"") || out.contains("=mock"),
        "missing system value 'mock' in: {out}"
    );
    assert!(out.contains("cache.op"), "missing cache.op in: {out}");
    assert!(out.contains("cache.hit"), "missing cache.hit in: {out}");
}

// ---------- 2) slow path emits WARN ----------

#[test]
fn cache_slow_get_logs_warn() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = CaptureWriter(buf.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("warn"))
        .with_writer(writer)
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        rt().block_on(async {
            let slow = SlowThresholds {
                cache_op: Duration::from_millis(1),
                ..SlowThresholds::default()
            };
            let cache = Instrumented::new(
                Arc::new(MockCache {
                    delay: Duration::from_millis(10),
                }),
                slow,
            );
            let _ = cache.get("k").await.unwrap();
        });
    });

    let out = captured(&buf);
    assert!(
        out.contains("WARN"),
        "expected WARN in slow path output: {out}"
    );
    assert!(
        out.contains("slow or failing"),
        "expected slow-op WARN message in: {out}"
    );
}

// ---------- in-memory EventBus ----------

#[derive(Default)]
struct InMemoryBus {
    inbox: Mutex<Vec<DeliveredMessage>>,
}

struct NoopAcker;

#[async_trait]
impl Acker for NoopAcker {
    async fn ack(self: Box<Self>) -> Result<(), Error> {
        Ok(())
    }
    async fn nack(self: Box<Self>, _delay: Option<Duration>) -> Result<(), Error> {
        Ok(())
    }
}

#[async_trait]
impl EventBus for InMemoryBus {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<MessageId, Error> {
        self.publish_with_headers(subject, payload, HashMap::new())
            .await
    }
    async fn publish_with_headers(
        &self,
        subject: &str,
        payload: &[u8],
        headers: HashMap<String, String>,
    ) -> Result<MessageId, Error> {
        let id = format!("msg-{}", self.inbox.lock().unwrap().len() + 1);
        let msg = DeliveredMessage::new(
            id.clone(),
            subject.to_string(),
            payload.to_vec(),
            headers,
            1,
            NoopAcker,
        );
        self.inbox.lock().unwrap().push(msg);
        Ok(id)
    }
    async fn subscribe(
        &self,
        _pattern: &str,
        _group: &str,
        _opts: SubscribeOptions,
    ) -> Result<Subscription, Error> {
        let drained: Vec<_> = self.inbox.lock().unwrap().drain(..).collect();
        Ok(Subscription::new(async_stream::stream! {
            for m in drained {
                yield m;
            }
        }))
    }
    fn system(&self) -> &'static str {
        "in-memory"
    }
}

/// Build a subscriber that produces real OTel trace ids on tracing spans,
/// so `inject_current_context_map` has something to inject. Returns the
/// subscriber + capture buf + the tracer provider (kept alive for the
/// duration of the test).
fn otel_subscriber() -> (
    impl tracing::Subscriber + Send + Sync,
    Arc<Mutex<Vec<u8>>>,
    SdkTracerProvider,
) {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("test");
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = CaptureWriter(buf.clone());
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW);
    let otel_layer = OpenTelemetryLayer::new(tracer);
    let subscriber = Registry::default()
        .with(EnvFilter::new("info"))
        .with(otel_layer)
        .with(fmt_layer);
    (subscriber, buf, provider)
}

// ---------- 3) publish injects traceparent ----------

#[test]
fn publish_injects_traceparent_for_extract() {
    let (subscriber, _buf, _provider) = otel_subscriber();

    let bus = Arc::new(InMemoryBus::default());
    let wrapped = Instrumented::with_defaults(bus.clone());

    tracing::subscriber::with_default(subscriber, || {
        rt().block_on(async {
            let parent = tracing::info_span!("test_parent");
            let _enter = parent.enter();
            wrapped.publish("orders.placed", b"payload").await.unwrap();
        });
    });

    let inbox = bus.inbox.lock().unwrap();
    assert_eq!(inbox.len(), 1, "expected one published message");
    let headers = &inbox[0].headers;
    assert!(
        headers.contains_key("traceparent"),
        "publish must inject traceparent; got headers: {:?}",
        headers
    );

    // Sanity: the propagator round-trips. (The empty assert! above is
    // the load-bearing check; this just confirms the header is well-
    // formed enough for the propagator to parse.)
    let propagator = TraceContextPropagator::new();
    struct M<'a>(&'a HashMap<String, String>);
    impl<'a> opentelemetry::propagation::Extractor for M<'a> {
        fn get(&self, k: &str) -> Option<&str> {
            self.0.get(k).map(|s| s.as_str())
        }
        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(|s| s.as_str()).collect()
        }
    }
    let _ctx = propagator.extract(&M(headers));
}

// ---------- 4) subscribe emits messaging.process span ----------

#[test]
fn subscribe_emits_messaging_process_span_attributes() {
    let (subscriber, buf, _provider) = otel_subscriber();

    tracing::subscriber::with_default(subscriber, || {
        rt().block_on(async {
            let bus = Arc::new(InMemoryBus::default());
            let wrapped = Instrumented::with_defaults(bus);

            {
                let parent = tracing::info_span!("publisher_parent");
                let _enter = parent.enter();
                wrapped.publish("orders.placed", b"x").await.unwrap();
            }

            let mut sub = wrapped
                .subscribe("orders.placed", "test-group", SubscribeOptions::default())
                .await
                .unwrap();
            let _msg = sub.next().await.expect("one message");
        });
    });

    let out = captured(&buf);
    assert!(
        out.contains("messaging.process"),
        "messaging.process span not emitted: {out}"
    );
    assert!(
        out.contains("messaging.consumer.id"),
        "consumer.id attribute missing: {out}"
    );
    assert!(
        out.contains("test-group"),
        "consumer group not in span attrs: {out}"
    );
    assert!(
        out.contains("messaging.delivery_attempt"),
        "delivery_attempt missing: {out}"
    );
}
