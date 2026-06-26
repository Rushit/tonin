// Request-header helpers for outbound gRPC calls.
//
// - `injectTraceparent` / `injectTracestate` — W3C distributed trace context
//   propagation so callees can join the caller's span.
// - `injectDeadline` — shrinking deadline propagation via the `grpc-timeout`
//   header so downstream hops never exceed the remaining budget.
//
// Each helper returns `true` if the header was injected, `false` if the value
// was invalid or the deadline already passed (so the caller can short-circuit).
// Mirrors `tonin_client.propagate` (Python) and `tonin-client::propagate` (Rust).

import { injectHeader, type OutboundMetadata } from "./_meta.js";

// W3C traceparent: `00-<32 hex>-<16 hex>-<2 hex>` — lowercase hex, version 00.
// https://www.w3.org/TR/trace-context/
const TRACEPARENT_RE = /^00-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$/;

/**
 * Inject a W3C `traceparent` header. Validates the format before injecting;
 * malformed values are dropped with a warning and `false` is returned.
 */
export function injectTraceparent(metadata: OutboundMetadata, traceparent: string): boolean {
  if (!TRACEPARENT_RE.test(traceparent)) {
    console.warn(`malformed traceparent ${JSON.stringify(traceparent)} — not injected`);
    return false;
  }
  injectHeader(metadata, "traceparent", traceparent);
  return true;
}

/**
 * Inject a W3C `tracestate` header (opaque vendor-specific trace data). No
 * validation beyond non-empty; returns `false` for empty strings.
 */
export function injectTracestate(metadata: OutboundMetadata, tracestate: string): boolean {
  if (!tracestate) return false;
  injectHeader(metadata, "tracestate", tracestate);
  return true;
}

/**
 * Inject a `grpc-timeout` header from a monotonic deadline.
 *
 * Computes the remaining budget as `deadlineMonotonicMs - performance.now()`
 * and encodes it in gRPC timeout format (e.g. `"450m"` for 450 ms). Returns
 * `false` without injecting when the deadline has already passed.
 *
 * @param deadlineMonotonicMs A `performance.now()`-based value (milliseconds)
 *   for when the call must complete, e.g. `performance.now() + timeoutMs`.
 */
export function injectDeadline(metadata: OutboundMetadata, deadlineMonotonicMs: number): boolean {
  const remainingMs = deadlineMonotonicMs - performance.now();
  if (remainingMs <= 0) return false;
  injectHeader(metadata, "grpc-timeout", formatGrpcTimeout(remainingMs / 1000));
  return true;
}

/**
 * Encode seconds as a gRPC timeout header value, picking the coarsest unit that
 * represents the duration without loss of precision.
 *
 * gRPC timeout units: `H` hours · `M` minutes · `S` seconds · `m` milliseconds ·
 * `u` microseconds · `n` nanoseconds.
 *
 * Exported for testing; not part of the package's public surface.
 */
export function formatGrpcTimeout(secs: number): string {
  const nanos = Math.round(secs * 1_000_000_000);
  if (nanos <= 0) return "0n";
  const units: Array<[number, string]> = [
    [3_600_000_000_000, "H"],
    [60_000_000_000, "M"],
    [1_000_000_000, "S"],
    [1_000_000, "m"],
    [1_000, "u"],
  ];
  for (const [divisor, unit] of units) {
    if (nanos >= divisor && nanos % divisor === 0) {
      return `${nanos / divisor}${unit}`;
    }
  }
  return `${nanos}n`;
}
