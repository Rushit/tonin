# Cache

A KV cache capability your handlers call through a trait — the backend is selected in `tonin.toml`.

## What you get

- A `Cache` trait you call from handlers (`get`, `set`, `set_nx`, `del`) — no Redis client types leak into your code.
- A `[cache]` section in `tonin.toml` that picks the implementation and references the cache the platform provides: a `REDIS_URL` env var from the platform's Secret and a network-policy egress rule to the cache pods. tonin does not run caches.
- Automatic telemetry on every call once you wrap your impl in `Instrumented<Cache>`: a `cache.op` span, a duration histogram, and a `WARN` log when an op crosses the slow-op threshold.
- Swap engines without touching handler code — change `engine = "..."` in `tonin.toml` and the dep in `Cargo.toml`.

See [01-principles.md](01-principles.md) for the interface-first rationale.

## Trait surface

From `tonin-core::traits::cache`:

```rust
#[async_trait]
pub trait Cache: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error>;

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), Error>;

    async fn del(&self, key: &str) -> Result<(), Error>;

    /// Conditional write — set only if the key doesn't exist.
    /// Returns `true` if written, `false` if the key already had a value.
    async fn set_nx(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool, Error>;

    /// `"redis" | "memcached" | "in-memory"` — emitted as the
    /// `cache.system` span attribute.
    fn system(&self) -> &'static str;
}
```

Values are raw bytes (`Vec<u8>` / `&[u8]`). Serialize at the call site — the trait does not assume a codec. TTL is per-call and optional; pass `None` for "no expiry" semantics matching the underlying store.

## Selecting an impl

`tonin.toml`:

```toml
[cache]
engine = "redis"   # only impl shipping today
```

`engine` picks the conceptual contract — once `tonin-redis` ships, the same entry will also pick
the runtime impl. In the Helm chart, `[cache]` is a **reference to the platform contract**: the
platform layer (a Redis manifest, or a managed Valkey/ElastiCache with OpenTofu-created Secrets)
provides, in the service's namespace:

| Contract | Default (`<svc>` = `[service].name`) |
|---|---|
| Secret with the connection URL | `<svc>-cache`, key `uri` |
| Cache pods (for network policy) | label `app.kubernetes.io/name=<svc>-cache`, port 6379 |

`tonin helm generate` renders `REDIS_URL` from `secretKeyRef` `<svc>-cache` / `uri` (on the
server container and the migration steps) and an egress rule to those pods. It renders no Redis
StatefulSet or Service; `generate` deletes the `cache-*.yaml` templates earlier versions shipped.

Every field can be overridden on `[cache]` and per environment in `[cache.<env>]`:

| Field | Default | Meaning |
|---|---|---|
| `engine` | (required) | `redis`, or `none` (no cache in this env) |
| `env` | `REDIS_URL` | env var the URL lands in |
| `secret` | `<svc>-cache` | Secret holding the URL |
| `key` | `uri` | key of the URL in `secret` |
| `labels` | `{ "app.kubernetes.io/name" = "<svc>-cache" }` | pods egress is allowed to; `{}` renders no rule |
| `namespace` | the service's | namespace of those pods (`{env}` is substituted) |
| `port` | 6379 | egress port |
| `url` | — | per env only: a literal URL instead of the Secret |

`zvectorlabs-api` reads `PLATFORM_REDIS_URL`:

```toml
[cache]
engine = "redis"
env    = "PLATFORM_REDIS_URL"     # Secret zvectorlabs-api-cache, key uri

[cache.dev]                        # dev cluster without contract Secrets
url       = "redis://redis.shared-dev.svc.cluster.local:6379"
namespace = "shared-dev"
labels    = { "app.kubernetes.io/name" = "redis" }
```

`[caches.<name>]` takes the same fields, with defaults prefixed by the name: env
`<NAME>_REDIS_URL`, Secret and pod label `<svc>-<name>-cache`.

Removed in 0.15, with an error naming the replacement: `shared` (every cache is a reference; use
a per-env `url` for a dev instance), `size` (the platform layer sizes the cache), `name` (use
`secret` and `labels`), and `url` on the base `[cache]` table.

See [12-kubernetes-deploy.md](12-kubernetes-deploy.md) for the rendered chart and
[08-database.md](08-database.md) for the database side of the contract.

## Using it

In 0.1 you wire `Arc<dyn Cache>` into your own handler-state struct. Once `tonin-redis` ships in 0.2, the builder will hand the wrapped impl back through a `Service` accessor.

```rust
use std::sync::Arc;
use std::time::Duration;
use tonin::core::traits::Cache;

#[derive(Clone)]
struct AppState {
    cache: Arc<dyn Cache>,
}

async fn get_profile(state: AppState, user_id: &str) -> Result<Profile, tonin::Error> {
    let key = format!("profile:{user_id}");

    if let Some(bytes) = state.cache.get(&key).await? {
        return Ok(serde_json::from_slice(&bytes)?);
    }

    let profile = load_from_db(user_id).await?;
    let bytes = serde_json::to_vec(&profile)?;
    state.cache
        .set(&key, &bytes, Some(Duration::from_secs(300)))
        .await?;

    Ok(profile)
}
# #[derive(serde::Serialize, serde::Deserialize)] struct Profile;
# async fn load_from_db(_: &str) -> Result<Profile, tonin::Error> { unimplemented!() }
```

No Redis types, no connection pool management, no client construction in the handler. The same handler compiles against any future `Cache` impl.

Full reference: the [greeter example](https://github.com/Rushit/tonin/tree/main/examples/greeter).

## Telemetry wrapper

Wrap your `Cache` impl once with `Instrumented::with_defaults(Arc::new(MyCache))` (from `tonin_core::instrumented`) to opt into the framework's telemetry. On each call the wrapper:

1. Opens a `cache.op` span tagged with `cache.system` (from `system()`), `cache.op` (the operation name — `get` / `set` / `set_nx` / `del`), and `cache.key.hash` (a salted hash; key strings are never recorded).
2. Records the call duration into the `cache_op_duration_ms` histogram and increments `cache_op_total` with an `outcome` label (`hit` / `miss` / `ok` / `error` / `exists`).
3. Emits a `WARN` log if the call exceeds `SlowThresholds::cache_op` (default **20ms**).

Thresholds come from `tonin_core::telemetry::capability_metrics::SlowThresholds`:

```rust
pub struct SlowThresholds {
    pub db_query: Duration,         // default 200ms
    pub cache_op: Duration,         // default 20ms
    pub messaging_publish: Duration, // default 50ms
}
```

Override at builder time if 20ms is too aggressive for your workload. See [05-telemetry.md](05-telemetry.md) for the broader span / metric story and OTLP export.

## Flow

```mermaid
flowchart LR
    H[Handler] -->|cache.get/set| I["Instrumented&lt;Cache&gt;"]
    I -->|delegated call| R[Redis Cache impl]
    R -->|RESP| B[(Redis backend)]
    I -. cache.op span<br/>histogram + counter<br/>slow-op WARN .-> O[OTel SDK]
```

The handler only sees the trait. `Instrumented` is invisible at the call site but emits one span and one histogram sample per call.

## Status (0.15)

- **Ships now** — `Cache` trait, `Instrumented<Cache>` decorator, slow-op thresholds, and metric handles all live in `tonin-sdk`. The `[cache]` TOML section references the platform's cache: `REDIS_URL` from the contract Secret, egress to the contract pod labels, per-env overrides and a dev `url`.
- **Not yet**
  - No `tonin-redis` impl crate. Services that need a cache implement `Cache` themselves (a few dozen lines of `redis-rs` glue) and wrap it in `Instrumented::with_defaults(...)`.
  - No `Service::with_cache` / `Service::cache()` accessor. Hold `Arc<dyn Cache>` in your own state struct.
  - Redis credentials, if any, are part of the contract Secret's `uri`.
- **Plan** — `tonin-redis` crate implementing both `Cache` and `EventBus`; builder accessor wires the configured engine.

## See also

- [01-principles.md](01-principles.md) — interface-first capabilities
- [08-database.md](08-database.md) — sibling capability, same instrumentation pattern
- [09-event-bus.md](09-event-bus.md) — sibling capability, same instrumentation pattern
