# tonin-client (TypeScript)

Client-side primitives shared between generated [tonin](https://github.com/Rushit/tonin)
service clients in TypeScript — the TS sibling of the Rust crate
[`tonin-client`](../../crates/tonin-client) and the Python package
[`python/tonin-client`](../../python/tonin-client).

Peer services that just want to **call** another service depend on this tiny,
**zero-runtime-dependency** package — not on the server framework. Everything
here is a contract between framework-owned middleware and hand-written caller
code, kept byte-for-byte compatible across the polyglot mesh.

## What's in it

| Module | Exports |
|--------|---------|
| `auth` | `AuthCtx`, `AuthError`, `PrincipalKind`, `RawToken`, `GrpcStatus` |
| `retry` | `RetryPolicy` (`none` / `exponential` / `fixed`), `Backoff` |
| `breaker` | `CircuitBreaker` (`default` / `aggressive` / `conservative`) |
| `propagate` | `injectTraceparent`, `injectTracestate`, `injectDeadline` |

```ts
import { AuthCtx, RetryPolicy } from "tonin-client";
import { GreeterClient, HelloRequest } from "greeter-client";

const metadata: Array<[string, string]> = [];
AuthCtx.fromBearer(myToken).propagate(metadata); // → authorization: Bearer …

const client = new GreeterClient(channel);
const reply = await client.sayHello(HelloRequest.create({ name: "world" }), { metadata });
```

`propagate` also accepts a `@grpc/grpc-js` `Metadata` object or a plain headers
record — the type is structural, so this package needs no dependency on grpc-js.

## Retries, caching, circuit breaking

The config types here describe behavior; **execution is delegated to the per-pod
outbound sidecar proxy** (`tonin-proxy`) so it works across all worker
processes. A TS client dials its sidecar over plain gRPC (HTTP/2) just like the
Python clients do — no special wiring in this package.

## Wire shape

Field names are idiomatic **camelCase**. The cross-language wire shape is
**snake_case** (matching Rust's serde output). Cross that boundary with
`AuthCtx.toWire()` / `AuthCtx.fromWire()`.

## Source of truth & drift gate

The Rust types in `crates/tonin-client/src/` are the source of truth.
`cargo run --bin gen-shared-types --features cli` regenerates `src/_generated.ts`
(the snake_case mirror). `test/generated-match.test.ts` fails if the
hand-written types here drift from that mirror.

## Tests

- **Unit** — `auth`, `retry`/`breaker`, `propagate`, plus the `generated-match`
  drift gate and a `wire-compat` round-trip.
- **`e2e`** — a real `@grpc/grpc-js` round-trip proving `AuthCtx.propagate` and
  the trace helpers flow over the genuine wire (grpc-js is a dev-only dep).
- **`sidecar`** — the production topology: client → **`tonin-proxy`** sidecar →
  upstream, asserting the proxy forwards the propagated `authorization` /
  `traceparent` verbatim. It auto-skips unless the proxy binary is present, so
  `npm test` stays hermetic. To run it locally:

  ```bash
  cargo build -p tonin-proxy          # from the repo root
  npm test                            # sidecar suite now executes
  # or point at a prebuilt binary:
  TONIN_PROXY_BIN=/path/to/tonin-proxy npm test
  ```

## Develop

```bash
npm install
npm run typecheck   # tsc, includes the type-level drift checks
npm test            # vitest (unit + e2e; sidecar if the proxy binary is built)
npm run build       # emit dist/ (esm + .d.ts)
```

Licensed under the [Apache License, Version 2.0](../../LICENSE).
