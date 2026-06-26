// Retry-policy configuration for outbound calls.
//
// The actual retry mechanism is delegated to the per-pod outbound sidecar
// proxy (`tonin-proxy`), so it works across all worker processes. The **config
// type lives here** so generated client SDKs can expose the knob to peer
// services without dragging in the framework.
//
// Default policy is **no retries**: retries change observable behavior
// (duplicated side effects, amplified load on a slow callee), so callers opt
// in explicitly. Same defaults as the Rust and Python sides.

/** Retry backoff strategy. Lowercase values match the Rust/Python enums. */
export enum Backoff {
  EXPONENTIAL = "exponential",
  FIXED = "fixed",
}

/**
 * Retry behavior for an outbound RPC.
 *
 * `retryableCodes` carries gRPC status code *names* (e.g. `"UNAVAILABLE"`).
 * It is intentionally not part of the cross-language generated shape — the
 * portable subset is the numeric knobs; codes stay language-local.
 */
export interface RetryPolicy {
  /** Total attempts including the first. `1` = no retry. */
  maxAttempts: number;
  /** Delay before the first retry, in seconds. */
  initialBackoffSecs: number;
  /** How subsequent delays grow. */
  backoff: Backoff;
  /** Exponential multiplier — ignored when `backoff === FIXED`. */
  multiplier: number;
  /** Cap on any single backoff interval, in seconds. */
  maxBackoffSecs: number;
  /** gRPC status code names that count as retryable. */
  retryableCodes: string[];
}

/**
 * Safe-only retryable codes: transient infra failures. Adding `INTERNAL` or
 * `ABORTED` should be a deliberate call — they often mean "the server thinks it
 * processed your request".
 */
function defaultRetryableCodes(): string[] {
  return ["UNAVAILABLE", "DEADLINE_EXCEEDED"];
}

// `RetryPolicy` is both a type (the interface above) and a value (the factory
// namespace below) via declaration merging — mirrors the Python classmethods.
export const RetryPolicy = {
  /** No retries — single attempt, fail fast. Default. */
  none(): RetryPolicy {
    return {
      maxAttempts: 1,
      initialBackoffSecs: 0,
      backoff: Backoff.FIXED,
      multiplier: 2.0,
      maxBackoffSecs: 1.0,
      retryableCodes: defaultRetryableCodes(),
    };
  },
  /** Exponential backoff with sane defaults: 50ms → 100ms → 200ms. */
  exponential(maxAttempts: number): RetryPolicy {
    return {
      maxAttempts,
      initialBackoffSecs: 0.05,
      backoff: Backoff.EXPONENTIAL,
      multiplier: 2.0,
      maxBackoffSecs: 2.0,
      retryableCodes: defaultRetryableCodes(),
    };
  },
  /** Fixed delay between attempts. */
  fixed(maxAttempts: number, delaySecs: number): RetryPolicy {
    return {
      maxAttempts,
      initialBackoffSecs: delaySecs,
      backoff: Backoff.FIXED,
      multiplier: 2.0,
      maxBackoffSecs: delaySecs,
      retryableCodes: defaultRetryableCodes(),
    };
  },
};
