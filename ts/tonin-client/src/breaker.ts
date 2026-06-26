// Circuit-breaker configuration for outbound calls.
//
// Same split as `retry`: the active mechanism (failure-rate tracking) is
// delegated to the per-pod outbound sidecar proxy, but the config type lives
// here so peers can tune the breaker without depending on the server framework.
//
// State machine: standard three-state breaker (Closed → Open → HalfOpen →
// Closed). Same defaults as the Rust and Python sides.

/**
 * Breaker config for outbound RPCs.
 *
 * - **Closed**: traffic flows; failures counted in a rolling window.
 * - **Open**: failure rate crossed `tripThreshold`; calls short-circuit with
 *   `UNAVAILABLE` for `resetAfterSecs`.
 * - **HalfOpen**: `halfOpenProbes` requests get through; any success → Closed,
 *   any failure → Open.
 *
 * `failureCodes` carries gRPC status code *names* and, like
 * `RetryPolicy.retryableCodes`, is language-local (not in the generated shape).
 */
export interface CircuitBreaker {
  /** Rolling window for failure-rate calculation, in seconds. */
  windowSecs: number;
  /** Failure ratio in `(0, 1]` at which the breaker trips. */
  tripThreshold: number;
  /** Minimum requests in the window before the breaker can trip. */
  minRequests: number;
  /** How long the breaker stays Open before HalfOpen, in seconds. */
  resetAfterSecs: number;
  /** Probe requests allowed through in HalfOpen. */
  halfOpenProbes: number;
  /** gRPC status code names that count as failures. */
  failureCodes: string[];
}

function defaultFailureCodes(): string[] {
  return ["UNAVAILABLE", "DEADLINE_EXCEEDED"];
}

export const CircuitBreaker = {
  /** Balanced defaults: trip at 50% failures over a 10s window. */
  default(): CircuitBreaker {
    return {
      windowSecs: 10,
      tripThreshold: 0.5,
      minRequests: 20,
      resetAfterSecs: 30,
      halfOpenProbes: 3,
      failureCodes: defaultFailureCodes(),
    };
  },
  /** Trip fast — for non-critical RPCs where fail-fast beats queuing. */
  aggressive(): CircuitBreaker {
    return {
      windowSecs: 5,
      tripThreshold: 0.3,
      minRequests: 5,
      resetAfterSecs: 15,
      halfOpenProbes: 1,
      failureCodes: defaultFailureCodes(),
    };
  },
  /** Trip slow — for critical-path RPCs where queuing beats shedding. */
  conservative(): CircuitBreaker {
    return {
      windowSecs: 30,
      tripThreshold: 0.75,
      minRequests: 50,
      resetAfterSecs: 60,
      halfOpenProbes: 5,
      failureCodes: ["UNAVAILABLE"],
    };
  },
};
