# tonin-client

Client-side primitives for code that **calls** a [tonin](https://crates.io/crates/tonin) service from another service.

Part of the [tonin](https://github.com/Rushit/tonin) framework.

## When to use this crate

You are writing service **B**, and B calls service **A** (which is built on tonin). You want:

- the same `AuthCtx` shape A produces, so you can forward the caller's identity to A
- W3C trace-context propagation, so A's spans join your trace
- retry / circuit-breaker config types, so the knobs match what A's generated client SDK accepts

Pull in `tonin-client`, **not** [`tonin-core`](https://crates.io/crates/tonin-core). The latter drags in the tonic server stack, the OTLP telemetry SDK, the MCP sidecar runtime, JWKS fetching, JWT validation, etc. — none of which a pure caller needs.

The dep tree here is intentionally tiny: `tonic`, `http`, `serde`, `serde_json`, `thiserror`, `tracing`. That's it.

## Quick example

```rust,no_run
use tonin_client::{AuthCtx, breaker::CircuitBreaker, propagate, retry::RetryPolicy};
use tonic::Request;

// In a handler on service B, you already have an AuthCtx (the inbound
// auth layer put it in the request extensions on the way in).
async fn forward(inbound: Request<()>) -> Result<(), tonic::Status> {
    let caller = AuthCtx::from(&inbound);

    // Build an outbound request to service A.
    let mut outbound = Request::new(());

    // 1. Forward the caller's bearer token so A sees the same identity.
    caller.propagate(&mut outbound);

    // 2. Inject W3C traceparent so A's spans join this trace.
    //    (In real code the traceparent comes from your tracing context;
    //    pre-formatted here for brevity.)
    propagate::inject_traceparent(
        &mut outbound,
        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
    );

    // 3. Configure retry + breaker on the generated client. The active
    //    layers live in tonin-core; the config types live here so the
    //    generated SDK can expose them without pulling the server in.
    let _retry = RetryPolicy::exponential(3);
    let _breaker = CircuitBreaker::default();
    // let client = ServiceAClient::connect("http://a:50051").await?
    //     .with_retry(_retry)
    //     .with_circuit_breaker(_breaker);
    // client.do_thing(outbound).await?;

    Ok(())
}
```

## Modules

- [`auth`](src/auth.rs) — `AuthCtx`, `RawToken`, `PrincipalKind`, `AuthError`. The auth-context shape produced server-side by `tonin-core::auth` and propagated outbound via `AuthCtx::propagate`.
- [`retry`](src/retry.rs) — `RetryPolicy`, `Backoff`, `RetryableCodes`. Config for the outbound retry layer. Default is no retries; opt in explicitly with `RetryPolicy::exponential(n)` or `::fixed(n, delay)`.
- [`breaker`](src/breaker.rs) — `CircuitBreaker` config. Standard three-state breaker (Closed / Open / HalfOpen). Presets: `default()`, `aggressive()`, `conservative()`.
- [`propagate`](src/propagate.rs) — `inject_traceparent` / `inject_tracestate` for W3C trace-context forwarding on outbound `tonic::Request`s.

## What is NOT in this crate

By design:

- JWT validation, JWKS fetching, the `TokenVerifier` trait, `AuthLayer` (server-side — in `tonin-core`)
- The actual retry / circuit-breaker tower layers (server-side — in `tonin-core`)
- Anything that opens a TCP listener or needs `tonic-build`
- OpenTelemetry SDK / OTLP exporter (server-side — in `tonin-telemetry`)

---

Licensed under the Apache License, Version 2.0.
