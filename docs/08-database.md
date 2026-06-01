# Database

A connection URL and a telemetry label — your DB client crate does the rest.

## What you get

- A `DATABASE_URL` env var injected into the service container at deploy time.
- A `db.system` label (`postgres` / `mysql` / `sqlite` / `clickhouse`) attached to spans so DB calls show up correctly in your tracing backend.
- A rendered StatefulSet + Service + Secret for the database when you declare one in `tonin.toml`.
- An optional migration init-container (or boot step) that runs before your service starts taking traffic.
- A `Database` trait you can implement to carry the connection URL + system label uniformly across services — typed access stays in your DB client crate (sqlx, sea-orm, tokio-postgres). The framework does not ship an ORM.

## Trait surface

From `crates/tonin-core/src/traits/database.rs`:

```rust
#[async_trait]
pub trait Database: Send + Sync + 'static {
    /// Connection URL — e.g. `postgres://user:pass@host:5432/db`.
    /// Default impls read this from the `DATABASE_URL` env var the
    /// deployment template injects.
    fn url(&self) -> &str;

    /// Optional framework-managed pool. Default `None`.
    fn pool(&self) -> Option<&dyn AnyPool> { None }

    /// Span attribute `db.system` — one of:
    /// "postgres" | "mysql" | "sqlite" | "clickhouse".
    fn system(&self) -> &'static str;
}
```

`AnyPool` is a marker trait reserved for a future framework-managed pool. Today, it returns `None` and you bring your own pool (sqlx::PgPool, etc.).

## Selecting an impl

Declare the database in `tonin.toml`:

```toml
[database]
engine = "postgres"
size   = "10Gi"
shared = false
```

What this drives at `tonin k8s generate`:

- `db-statefulset.yaml` — Postgres StatefulSet with a 10Gi PVC
- `db-service.yaml` — headless Service in front of the StatefulSet
- `db-secret.yaml` — placeholder Secret holding `DATABASE_PASSWORD`
- `deployment.yaml` — your service container picks up:
  - `DATABASE_URL = postgres://<svc>:$DATABASE_PASSWORD@<svc>-db.<ns>.svc.cluster.local:5432/<svc>` (literal)
  - `DATABASE_PASSWORD` (from the rendered Secret via `secretKeyRef`)

Set `shared = true` plus `name` + `namespace` to point at an existing cluster Postgres instead of provisioning a new StatefulSet. Per-environment overlays (`[database.dev]`, `[database.prod]`) override fields per env.

Supported engines today: `postgres`, `mysql`, `sqlite`, `clickhouse`. Defaults: Postgres 18, MySQL 8, ClickHouse 24.3.

## Using it from sqlx

The framework hands you a URL — you build the pool and thread it through your own handler-state struct, then hand the tonic-generated server to `Service::handler`.

```rust,no_run
use tonin::prelude::*;
use sqlx::postgres::PgPoolOptions;

#[derive(Clone)]
struct AppState {
    db: sqlx::PgPool,
}

#[tokio::main]
async fn main() -> tonin::Result<()> {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL set by the rendered Deployment");
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&url)
        .await
        .expect("connect to postgres");

    let _state = AppState { db };
    // let billing_server = BillingServer::new(BillingImpl::new(state));

    Service::new("billing")
        // .handler(billing_server)
        .run()
        .await
}
```

`Service::new` installs the OTel tracer provider (see [05-telemetry.md](05-telemetry.md)). sqlx emits its own spans when an OTel provider is registered — those spans automatically join the request trace, so a single gRPC call shows up as `handler → sqlx::query → postgres`.

## Migrations

Add a `[migrations]` block to control when and how migrations run:

```toml
[migrations]
tool   = "sqlx"            # "sqlx" | "refinery" | "flyway" | "custom"
dir    = "migrations/"     # default
run_on = "init-container"  # "init-container" (default) | "boot" | "manual"
```

What each `run_on` mode does:

- `init-container` — the renderer adds an initContainer to your Deployment that runs the migration command before the service container starts. Safest default; failed migrations block rollout.
- `boot` — the migration command runs as part of your service binary's startup. You wire the call yourself.
- `manual` — no automation; you run migrations out-of-band (Job, kubectl exec, GitOps tool).

For `tool = "custom"`, supply the command:

```toml
[migrations]
tool    = "custom"
command = ["./migrate.sh", "--all"]
run_on  = "init-container"
```

Confirmed against `MigrationsSpec` in `crates/tonin/src/codegen/stateful.rs`:

```rust
pub struct MigrationsSpec {
    pub tool: MigrationTool,        // Sqlx | Refinery | Flyway | Custom
    pub dir: String,
    pub run_on: MigrationRunOn,     // InitContainer | Boot | Manual
    pub command: Vec<String>,       // computed for known tools
}
```

Known tools have their command pre-computed — e.g. sqlx becomes `["sqlx", "migrate", "run", "--source", "migrations/"]`.

## Status (0.1)

- **Ships now** — `Database` trait + `AnyPool` marker in `tonin-core::traits`; the `[database]` + `[migrations]` TOML sections are parsed, render the StatefulSet / Service / Secret manifests, and inject `DATABASE_URL` + `DATABASE_PASSWORD` env vars. `[database.<env>]` overlays are honored.
- **Not yet in 0.1**
  - No `Service::with_database` / `Service::database()` accessor; hold your sqlx pool in your own handler-state struct.
  - `AnyPool::pool()` always returns `None`; there is no framework-managed pool.
  - No `tonin-postgres` impl crate. Today the value of `system()` is the only thing other code reads.
- **0.2 plan** — `tonin-postgres` (and friends) implementing `Database` and exposing a managed `sqlx` pool; `Service` accessor wires it.

## Flow

```mermaid
flowchart LR
    H[gRPC handler] -->|state.db.acquire| P[sqlx Pool]
    P -->|TCP 5432| S[(Postgres StatefulSet)]
    H -.->|span: rpc.method| T[OTel tracer]
    P -.->|span: db.statement<br/>db.system=postgres| T
    T -->|OTLP| C[Collector]
```

The handler span and sqlx's auto-emitted DB spans share the same trace context because `Service::new` installed the global tracer provider. The collector sees the parent-child link without any per-call wiring.

## See also

- [07-cache.md](07-cache.md) — same `engine = "..."` pattern for Redis
- [12-kubernetes-deploy.md](12-kubernetes-deploy.md) — how `[database]` becomes a StatefulSet
- Full example: https://github.com/Rushit/tonin/tree/main/examples/greeter
