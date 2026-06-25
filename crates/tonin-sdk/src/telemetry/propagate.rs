//! W3C TraceContext propagation across gRPC boundaries.
//!
//! - [`extract_layer`] — server side. Wraps every incoming request: reads
//!   `traceparent` (and any other configured propagators) from headers,
//!   binds the resulting OTel context as the parent of the request's
//!   tracing span.
//! - [`inject_current_context`] — client side. Call before sending a tonic
//!   request to add `traceparent` to its metadata so the downstream service
//!   sees this service's current span as the parent.

use std::collections::HashMap;
use std::task::{Context as TaskContext, Poll};

use futures_util::future::BoxFuture;
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response};
use opentelemetry::Context;
use opentelemetry::global;
use opentelemetry::propagation::{Extractor, Injector};
use tower::{Layer, Service};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

// ---------------- server side: extract ----------------

/// A `tower::Layer` that, for each incoming HTTP/gRPC request, extracts the
/// W3C context from request headers and attaches it to the current `tracing`
/// span as its parent.
///
/// Install once in `Service::new` so all RPCs benefit.
pub fn extract_layer() -> ExtractLayer {
    ExtractLayer
}

#[derive(Clone, Copy, Default)]
pub struct ExtractLayer;

impl<S> Layer<S> for ExtractLayer {
    type Service = ExtractService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        ExtractService { inner }
    }
}

#[derive(Clone)]
pub struct ExtractService<S> {
    inner: S,
}

impl<S, B, Resp> Service<Request<B>> for ExtractService<S>
where
    S: Service<Request<B>, Response = Response<Resp>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let parent_cx =
            global::get_text_map_propagator(|prop| prop.extract(&HeaderExtractor(req.headers())));
        // Bind the extracted context to the *current* tracing span. Anything
        // the handler does inside it becomes a child of the caller's span.
        let _ = Span::current().set_parent(parent_cx);

        let mut inner = self.inner.clone();
        Box::pin(async move { inner.call(req).await })
    }
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl<'a> Extractor for HeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

// ---------------- client side: inject ----------------

/// Inject the current tracing span's W3C context into the given request's
/// metadata. Call from generated client code before sending each RPC:
///
/// ```ignore
/// let mut req = tonic::Request::new(payload);
/// tonin::telemetry::propagate::inject_current_context(req.metadata_mut());
/// client.do_thing(req).await
/// ```
pub fn inject_current_context(metadata: &mut tonic::metadata::MetadataMap) {
    let cx = Span::current().context();
    let mut injector = MetadataInjector(metadata);
    global::get_text_map_propagator(|prop| prop.inject_context(&cx, &mut injector));
}

struct MetadataInjector<'a>(&'a mut tonic::metadata::MetadataMap);

impl<'a> Injector for MetadataInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        if let Ok(name) = key.parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
            && let Ok(val) = value.parse()
        {
            self.0.insert(name, val);
        }
    }
}

/// Same as `inject_current_context` but operates on raw `http::HeaderMap`
/// (e.g., for non-tonic HTTP clients).
pub fn inject_current_context_http(headers: &mut HeaderMap) {
    let cx = Context::current();
    let mut injector = HttpInjector(headers);
    global::get_text_map_propagator(|prop| prop.inject_context(&cx, &mut injector));
}

struct HttpInjector<'a>(&'a mut HeaderMap);

impl<'a> Injector for HttpInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(val)) = (key.parse::<HeaderName>(), HeaderValue::from_str(&value)) {
            self.0.insert(name, val);
        }
    }
}

// ---------------- bus side: string-keyed map carrier ----------------
//
// `EventBus` impls carry headers as `HashMap<String, String>` on the wire
// (Redis Streams, NATS, Kafka all do this naturally). These helpers let
// the `Instrumented<EventBus>` decorator inject on publish and extract
// on per-message processing without each backend touching W3C semantics.

/// Inject the current tracing span's W3C context into a string-keyed
/// header map. The same `traceparent` (and any other globally-configured
/// W3C propagators) that gRPC clients inject will appear here.
pub fn inject_current_context_map(headers: &mut HashMap<String, String>) {
    let cx = Span::current().context();
    let mut injector = MapInjector(headers);
    global::get_text_map_propagator(|prop| prop.inject_context(&cx, &mut injector));
}

/// Build an OTel `Context` from W3C headers in a string-keyed map.
/// Caller binds it via `Span::current().set_parent(ctx)` before doing the
/// work whose span should be parented to it.
///
/// If no `traceparent` is present (or it's malformed), the returned
/// `Context` is the empty root — bind it and your span becomes a new
/// root, which is the right behavior (we never silently graft to
/// whatever happens to be `Span::current()`).
pub fn extract_context_from_map(headers: &HashMap<String, String>) -> Context {
    global::get_text_map_propagator(|prop| prop.extract(&MapExtractor(headers)))
}

struct MapInjector<'a>(&'a mut HashMap<String, String>);

impl<'a> Injector for MapInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
}

struct MapExtractor<'a>(&'a HashMap<String, String>);

impl<'a> Extractor for MapExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}
