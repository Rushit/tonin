# Cache

A KV cache capability your handlers call through a trait — the backend is selected in `tonin.toml`.

## What you get

- A `Cache` trait you call from handlers (`get`, `set`, `set_nx`, `del`) — no Redis client types leak into your code.
- A `[cache]` section in `tonin.toml` that picks the implementation and renders the matching k8s manifests — a Redis `StatefulSet` + headless `Service` for owned mode, or just a `REDIS_URL` env var for shared mode.
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
engine = "redis"   # only impl shipping in 0.2
shared = false     # true to point at a cluster-wide Redis instead of a per-service StatefulSet
```

`engine` drives both the rendered k8s manifests and the conceptual contract — once `tonin-redis` ships in 0.2, the same TOML entry will also pick the runtime impl. With `shared = false`, the CLI renders a dedicated Redis `StatefulSet` + `Service` alongside your Deployment and injects a `REDIS_URL` env var. With `shared = true`, no StatefulSet is rendered; the renderer only injects the `REDIS_URL` env var pointing at the conventional in-cluster name, and you bring the Redis yourself.

See [12-kubernetes-deploy.md](12-kubernetes-deploy.md) for the rendered manifest shape.

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

## Status (0.1)

- **Ships now** — `Cache` trait, `Instrumented<Cache>` decorator, slow-op thresholds, and metric handles all live in `tonin-core` today. The `[cache]` TOML section is parsed and renders a Redis `StatefulSet` + `Service` via the k8s templates; `REDIS_URL` lands as an env var on the Deployment.
- **Not yet in 0.1**
  - No `tonin-redis` impl crate. Services that need a cache implement `Cache` themselves (a few dozen lines of `redis-rs` glue) and wrap it in `Instrumented::with_defaults(...)`.
  - No `Service::with_cache` / `Service::cache()` accessor. Hold `Arc<dyn Cache>` in your own state struct.
  - No cache `Secret` is rendered (only `db-secret.yaml` ships for `[database]`). If your Redis requires auth, supply it via your own Secret + env var.
- **0.2 plan** — `tonin-redis` crate implementing both `Cache` and `EventBus`; builder accessor wires the configured engine.

## See also

- [01-principles.md](01-principles.md) — interface-first capabilities
- [08-database.md](08-database.md) — sibling capability, same instrumentation pattern
- [09-event-bus.md](09-event-bus.md) — sibling capability, same instrumentation pattern
