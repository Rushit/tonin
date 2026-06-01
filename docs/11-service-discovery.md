# Service discovery

Reach peer services by name. No client config, no registry, no sidecar handshake.

## What you get

- **No client config.** Service B reaches service A by its logical name — `"inventory"` in namespace `"shop"` — and gets back a URL ready to feed into a generated gRPC client.
- **One function call.** `tonin::discovery::service_url("inventory", "shop")` returns `http://inventory.shop.svc.cluster.local:50051`.
- **No registry to run.** Kubernetes already runs DNS. The discovery module is a URL builder over the cluster's DNS naming convention; there is no Consul, no etcd-backed registry, no client-side load balancer to keep alive.
- **Mesh handles the hard parts.** Cross-cluster routing, mTLS, retries, and circuit breaking sit in the mesh (Linkerd / Istio / Cilium). Calling code stays the same whether the peer is in the same namespace, another namespace, or another cluster.
- **Trace propagation works through it.** Because the URL is consumed by a normal tonic client, `traceparent` injection happens on the way out and the receiver picks it up — see [05-telemetry.md](05-telemetry.md).

## How it works

`tonin-core::discovery` is a thin URL builder. It encodes one convention — k8s `Service` DNS — and stops there.

```rust
pub fn service_url(name: &str, namespace: &str) -> String {
    format!("http://{name}.{namespace}.svc.cluster.local:50051")
}
```

The real resolution is done by the cluster:

1. The Kubernetes `Service` named `inventory` in namespace `shop` exposes a stable virtual IP.
2. CoreDNS (or whatever the cluster's DNS provider is) resolves `inventory.shop.svc.cluster.local` to that IP.
3. The mesh data plane (Linkerd proxy, Istio sidecar, Cilium eBPF) intercepts the connection, applies mTLS, does load balancing across the `Service`'s endpoints, and applies any retry / timeout policy attached to the route.

Cross-cluster routing is also the mesh's job — Linkerd multi-cluster gateways, Istio multi-mesh, and Cilium cluster mesh all make the same `*.svc.cluster.local` name resolve to a peer cluster's endpoint when configured. The framework does not need to know.

## Example

Service B (`orders`) calling service A (`inventory`):

```rust
use tonin::prelude::*;

#[tokio::main]
async fn main() -> tonin::Result<()> {
    let svc = Service::new("orders"); // installs telemetry + propagation

    let url = tonin::discovery::service_url("inventory", "shop");
    let mut client = InventoryClient::connect(url).await?;
    client.get_stock(req).await?;

    svc.run().await
}
```

The actual `examples/orders/src/main.rs` keeps it minimal — it just builds the URL and logs it:

```rust
use tonin::prelude::*;

#[tokio::main]
async fn main() -> tonin::Result<()> {
    let svc = Service::new("orders");
    let inventory_url = tonin::discovery::service_url("inventory", "shop");
    tracing::info!(%inventory_url, "orders will call inventory at this URL");
    svc.run().await
}
```

See [examples/orders](https://github.com/Rushit/tonin/tree/main/examples/orders) and the canonical [examples/greeter](https://github.com/Rushit/tonin/tree/main/examples/greeter).

## Cross-cluster

The same call works across clusters. You do not change the code, the URL, or the namespace.

When the mesh is configured for multi-cluster — Linkerd gateways, Istio east-west gateway, or Cilium cluster mesh — `inventory.shop.svc.cluster.local` resolves to a mirror `Service` that forwards to the remote cluster's `inventory` endpoint. The handler in B is unaware. Failover, locality-aware routing, and mTLS across the wire are all configured on the mesh, not in tonin.

See [13-service-mesh.md](13-service-mesh.md) for selecting and configuring a mesh.

## Port convention

The URL builder hard-codes port **50051**, the gRPC convention used across tonin templates, the `Dockerfile.tmpl`, and the generated k8s `Service` manifests. As long as every service uses the framework defaults, the port matches and `service_url` works without configuration.

A per-service override via `[deploy].port` in `tonin.toml` is planned but not yet wired through. Until then, if you must change the port, you build the URL yourself:

```rust
let port = 50052;
let url = format!("http://inventory.shop.svc.cluster.local:{port}");
```

## Resolution flow

```mermaid
flowchart LR
    B["Service B<br/>discovery::service_url(\"A\",\"ns\")"] --> URL["http://A.ns.svc.cluster.local:50051"]
    URL --> DNS["CoreDNS<br/>(k8s cluster DNS)"]
    DNS --> VIP["Service VIP<br/>A.ns"]
    VIP --> MESH["Mesh sidecar<br/>mTLS + retry + LB"]
    MESH --> A["Service A pod<br/>:50051"]
```

The framework's role ends at producing the URL. Everything after that — name resolution, transport security, load balancing, cross-cluster routing — is delegated to the platform.

## See also

- [13-service-mesh.md](13-service-mesh.md) — mesh selection and configuration; where cross-cluster routing and mTLS actually live.
- [12-kubernetes-deploy.md](12-kubernetes-deploy.md) — how the `Service` and `Deployment` manifests that make these names resolvable get rendered from `tonin.toml`.
- [05-telemetry.md](05-telemetry.md) — `traceparent` propagation across the discovered hop, so a call into A from B stays in the same trace.
