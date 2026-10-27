# tonin-proxy

Outbound gRPC proxy sidecar for non-Rust tonin services (Python, TypeScript, …).

Auto-injected by `tonin helm generate` when `language != rust` and `[depends_on]` is non-empty.

## What it does

The app connects to `localhost:6565` instead of the upstream service directly.
The proxy handles:

- **Singleflight coalescing** — concurrent calls with the same method + body share one upstream RPC across all worker processes.
- **TTL response cache** — successful responses are cached per method, configurable via `TONIN_PROXY_CACHE_*` env vars.
- **Retry with backoff** — transient upstream errors are retried with exponential backoff.
- **Circuit breaker** — repeated upstream faults (UNAVAILABLE, INTERNAL, UNKNOWN, DEADLINE_EXCEEDED, DATA_LOSS) open the breaker and fail fast until the upstream recovers. Application errors such as NOT_FOUND do not count.
- **Connection pooling** — a fixed pool of HTTP/2 connections to the upstream, multiplexed and reused round-robin. Closed connections are reconnected on next use.
- **HTTP/2 keep-alives** — PINGs on every connection (including idle ones) keep it from being pruned and detect dead connections instead of hanging on them.
- **Graceful shutdown** — on SIGTERM the proxy keeps serving for a short delay while the app shuts down, then sends GOAWAY and waits for in-flight requests.

Upstream gRPC errors are forwarded with their original `grpc-status`, `grpc-message`, and `grpc-status-details-bin`, and are never cached.

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
| `TONIN_PROXY_POOL_SIZE` | `8` | HTTP/2 connections kept to the upstream |
| `TONIN_PROXY_KEEPALIVE_MS` | `30000` | Interval between HTTP/2 keep-alive PINGs (`0` = disabled) |
| `TONIN_PROXY_KEEPALIVE_TIMEOUT_MS` | `10000` | Close a connection whose PING is not acked within this time |
| `TONIN_PROXY_SHUTDOWN_DELAY_MS` | `5000` | Keep serving this long after SIGTERM before draining |
