# Examples

A mini e-commerce topology showing tonin across languages, pod sizes, and
the default Cilium service mesh.

> **Note on layout.** These examples use a flat single-crate layout
> (`Cargo.toml` + `src/main.rs` + `proto/`) from an earlier scaffold era.
> Running `tonin service new <name>` today produces a richer
> multi-crate tree (`server/` + `client-rust/` + `proto/` + `Dockerfile`
> + pre-rendered `k8s/`) — see `crates/tonin/templates/service/rust/` for
> the canonical shape. The examples remain useful for cross-service
> mesh / discovery patterns (`orders` calling `inventory`).

```
            ┌────────────┐   gRPC-Web/Connect    ┌──────────────┐
 browser ── │  web-ui    │ ─────────────────────▶│   orders     │
            │  (TS, 50m) │                       │  (Rust,250m) │
            └────────────┘                       └──────┬───────┘
                                                        │ gRPC, WireGuard-encrypted by Cilium
                          ┌─────────────────────────────┼────────────────────┐
                          ▼                             ▼                    ▼
                  ┌──────────────┐              ┌──────────────┐    ┌────────────────┐
                  │  inventory   │              │  notifier    │    │  greeter       │
                  │ (Rust, 500m) │              │ (Python,50m) │    │ (Rust, 100m)   │
                  │   1Gi RAM    │              │  64Mi RAM    │    │   demo svc     │
                  └──────────────┘              └──────────────┘    └────────────────┘
```

| Service     | Language   | CPU req | Mem req | Replicas | MCP sidecar | Mesh   |
| ----------- | ---------- | ------- | ------- | -------- | ----------- | ------ |
| `greeter`   | Rust       | 100m    | 128Mi   | 2        | yes         | cilium |
| `orders`    | Rust       | 250m    | 256Mi   | 2        | yes         | cilium |
| `inventory` | Rust       | 500m    | 1Gi     | 3        | yes         | cilium |
| `notifier`  | Python 3.12| 50m     | 64Mi    | 2        | yes         | cilium |
| `web-ui`    | TS + React | 50m     | 64Mi    | 2        | no          | cilium |

## Service mesh: Cilium by default, swappable

micro picks **Cilium** as the default mesh: no per-pod sidecar (so we
don't stack one on top of the MCP container), kernel-level mTLS via WireGuard,
and L3-L7 NetworkPolicy in one model. CNCF graduated, Apache-2.0, free.

You can swap per service in `tonin.toml`:

```toml
[deploy]
mesh = "cilium"   # cilium | istio | linkerd | none
```

What changes when you swap:

| Mesh    | What gets rendered                                                  | Cluster prereq            |
| ------- | ------------------------------------------------------------------- | ------------------------- |
| cilium  | `io.cilium/encryption` annotation + `CiliumNetworkPolicy`           | Cilium CNI installed      |
| istio   | `ambient.istio.io/redirection` annotation + `AuthorizationPolicy` + `PeerAuthentication` | Istio ambient installed   |
| linkerd | `linkerd.io/inject` annotation + `Server` + `AuthorizationPolicy`   | Linkerd installed         |
| none    | Just a marker annotation; no mTLS, no policy                        | Local dev only            |

The Deployment, Service, and HPA YAML are mesh-agnostic — only files under
`templates/k8s/mesh/<mesh>/` differ.

## Zero-trust by default

Whichever mesh you pick, the CLI renders a **default-deny ingress policy**
and allows only the callers each service declared in `[depends_on]`. Example:

```toml
# examples/orders/tonin.toml
[depends_on]
inventory = "shop"
notifier  = "shop"
```

This produces a `CiliumNetworkPolicy` (or Istio `AuthorizationPolicy`) on
`inventory` and `notifier` that allows ingress from `orders` and nobody else
(plus the OTel collector, which is always allowed for tracing).

When a dependency's namespace differs per environment, use the `{env}`
placeholder or the table form (`inventory = { namespace = "shop-{env}", prod = "shop" }`) —
see [per-environment namespaces and dependencies](../docs/12-kubernetes-deploy.md#per-environment-namespaces-and-dependencies).

## Cross-language story

Every service is a `.proto` file. The codegen path that runs today is
**prost** (Rust, via `tonic-build`) and `grpcio-tools` (Python); TypeScript
uses `@bufbuild/protoc-gen-es` + `connect-es`. A buffa-based
`protoc-gen-micro` codegen plugin is reserved for the future via the
`[service].codec` field and `TONIN_CODEC` env var, but in 0.x both
`prost` and `buffa` values route through tonic-build / grpcio-tools.

Services interoperate at the wire: any pod in the same Cilium ClusterMesh
can call any other by DNS name, with encryption for free.

## Pod-size knobs

Each service's `tonin.toml` declares `[resources]` + `[autoscale]`. The
CLI renders these into the Deployment and HPA. Independent values per
service, independent deploy cadences, independent images.

## Running locally

End-to-end run requires: a kind/k3d cluster with **Cilium as CNI** (or another
supported mesh), an image registry the cluster can pull from. Each example
dir is independently buildable — they share only the `.proto` contract.
