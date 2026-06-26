// Public surface of `tonin-client` (TypeScript).
//
// The shared client-side primitives for generated tonin service clients —
// matching the Rust (`crates/tonin-client`) and Python (`python/tonin-client`)
// packages byte-for-byte on the wire. Peer services that just want to call
// another service depend on this package, NOT on the server framework.
//
// `_generated.ts` is intentionally NOT re-exported: it is the snake_case
// codegen mirror used only by the drift test (see `test/generated-match.test.ts`).

export { AuthCtx, AuthError, GrpcStatus, PrincipalKind } from "./auth.js";
export type { AuthCtxInit, AuthCtxWire, RawToken } from "./auth.js";

export { Backoff, RetryPolicy } from "./retry.js";

export { CircuitBreaker } from "./breaker.js";

export { injectDeadline, injectTraceparent, injectTracestate } from "./propagate.js";

export type { MetadataLike, OutboundMetadata } from "./_meta.js";
