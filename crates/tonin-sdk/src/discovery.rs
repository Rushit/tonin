//! tonin-discovery: build endpoint URLs from service names.
//!
//! Convention: `service_url("greeter", "default")` →
//! `http://greeter.default.svc.cluster.local:50051`.
//! Linkerd multi-cluster gateways make this resolve cross-cluster transparently.

pub fn service_url(name: &str, namespace: &str) -> String {
    format!("http://{name}.{namespace}.svc.cluster.local:50051")
}
