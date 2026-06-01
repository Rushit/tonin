# tonin-core

Core types and runtime for [tonin](https://crates.io/crates/tonin)
services: `Service` builder, capability traits, auth, MCP, telemetry,
transport, discovery.

Most users should depend on the umbrella crate
[`tonin`](https://crates.io/crates/tonin), which re-exports this
crate plus a curated prelude. Pull `tonin-core` in directly when you
want:

- fewer transitive deps than the umbrella
- direct control over feature gates (e.g. disabling MCP or telemetry
  once that becomes possible in 0.2)
- to build a `Service` without any of the optional sugar

## Quick example

```rust,no_run
use tonin_core::{Service, Result};

#[tokio::main]
async fn main() -> Result<()> {
    Service::new("greeter")
        .without_auth()
        // .handler(my_grpc_service)  // tonic-generated service
        .run()
        .await
}
```

With auth and MCP:

```rust,no_run
use tonin_core::{Service, Result};
use tonin_core::auth::default::JwtValidator;

#[tokio::main]
async fn main() -> Result<()> {
    Service::new("greeter")
        .with_auth(JwtValidator::from_env()?)
        .enable_mcp()
        // .handler(...)
        .run()
        .await
}
```

## Module map

| Module          | What's in it                                                                 |
|-----------------|------------------------------------------------------------------------------|
| `auth`          | `TokenExtractor`, `TokenVerifier`, `AuthLayer`, `AuthCtx`, `CURRENT_AUTH`    |
| `mcp`           | In-process MCP sidecar — `McpConfig`, `McpServerHandler`, `spawn` / `spawn_with` |
| `telemetry`     | Zero-config OTLP tracing init + W3C TraceContext propagation                 |
| `transport`     | tonic/gRPC wiring helpers used by `Service`                                  |
| `discovery`     | k8s DNS-based service resolution (`<svc>.<ns>.svc.cluster.local`)            |
| `traits`        | Capability traits — `Cache`, `Database`, `EventBus`, `SecretStore`           |
| `state`         | Pre-wired DB + cache connection handles loaded from env at boot              |
| `instrumented`  | Decorator wrappers that add OTel spans around capability impls               |
| `job`           | Background job / queue consumer surface (auth-aware via `service_token`)     |
| `error`         | `Error` + `Result` used across the crate                                     |

## Capability traits

The capability traits in [`traits`](src/traits/) are the load-bearing
"interface-first" surface — see
[`docs/01-principles.md`](https://github.com/Rushit/tonin/blob/main/docs/01-principles.md)
"Interface-first capabilities".

| Trait         | Purpose                                                       |
|---------------|---------------------------------------------------------------|
| `Cache`       | KV read/write/delete with TTL + conditional `set_nx`          |
| `Database`    | Connection URL + telemetry label (typed access via sqlx etc.) |
| `EventBus`    | At-least-once publish/subscribe with explicit ack             |
| `SecretStore` | Resolve secrets by key (default `EnvSecretStore`)             |

Concrete impls live in their own crates (`tonin-redis` ships
`Cache` + `EventBus`; future `tonin-postgres`, `tonin-nats`).
`tonin.toml` selects which impl to wire via `engine = "..."`:

```toml
[cache]
engine = "redis"

[database]
engine = "postgres"
```

Swapping Redis -> NATS for events is a TOML change + a `Cargo.toml` dep
flip, never a handler rewrite.

## Design caveat (0.1)

In 0.1, the auth defaults (`jsonwebtoken` + `reqwest` for JWKS fetch)
and the pre-wired state helpers (`sqlx` + `redis`) ship inside this
crate. The trait-only-core split is planned for 0.2 — see
[`docs/01-principles.md`](https://github.com/Rushit/tonin/blob/main/docs/01-principles.md).
Today `tonin-core` pulls those deps transitively; tomorrow they move
out to:

- `tonin-postgres` — `Database` impl + sqlx wiring
- `tonin-redis` — `Cache` + `EventBus` impls + redis wiring
- `tonin-auth-jwt` — `JwtValidator` + JWKS-fetching `TokenVerifier`

The trait surface in [`traits`](src/traits/) and [`auth`](src/auth/)
will stay stable across that move; only the `Cargo.toml` deps of
`tonin-core` shrink.

## License

Licensed under the Apache License, Version 2.0. See
[LICENSE](https://github.com/Rushit/tonin/blob/main/LICENSE).
