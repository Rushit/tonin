# tonin-proxy

Outbound gRPC proxy sidecar for non-Rust tonin services (Python, TypeScript, …).

Auto-injected by `tonin helm generate` when `language != rust` and `[depends_on]` is non-empty.

## What it does

The app connects to `localhost:6565` instead of the upstream service directly.
The proxy handles:

- **Singleflight coalescing** — concurrent calls with the same method + body share one upstream RPC across all worker processes.
- **TTL response cache** — successful responses are cached per method, configurable via `TONIN_PROXY_CACHE_*` env vars.
- **Retry with backoff** — transient upstream errors are retried with exponential backoff.
- **Circuit breaker** — repeated failures open the breaker and fail fast until the upstream recovers.

All W3C headers (`traceparent`, `tracestate`, `baggage`, `grpc-timeout`) are forwarded verbatim — the proxy is transparent to the app.

## Configuration

All config is via environment variables (set by `tonin helm generate`):

| Variable | Default | Description |
|---|---|---|
| `TONIN_PROXY_PORT` | `6565` | Port the proxy listens on |
| `TONIN_PROXY_UPSTREAM` | required | Upstream gRPC address (`http://localhost:50051`) |
| `TONIN_PROXY_LOG` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`) |
| `TONIN_PROXY_CACHE_TTL_MS` | `0` | Per-method cache TTL in ms (`0` = disabled) |
| `TONIN_PROXY_CACHE_CAPACITY` | `1000` | Max cached entries |
| `TONIN_PROXY_RETRY_MAX` | `3` | Max upstream retry attempts |
| `TONIN_PROXY_BREAKER_THRESHOLD` | `0.5` | Error ratio that trips the breaker |
