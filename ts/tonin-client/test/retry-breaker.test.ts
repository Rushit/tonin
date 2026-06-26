// Unit tests for retry + breaker config types — mirrors
// python/tonin-client/tests/test_retry_breaker.py.

import { describe, expect, it } from "vitest";

import { Backoff, CircuitBreaker, RetryPolicy } from "../src/index.js";

describe("RetryPolicy", () => {
  it("default / none is no retry", () => {
    expect(RetryPolicy.none().maxAttempts).toBe(1);
  });

  it("exponential has sane defaults", () => {
    const p = RetryPolicy.exponential(3);
    expect(p.maxAttempts).toBe(3);
    expect(p.backoff).toBe(Backoff.EXPONENTIAL);
    expect(p.multiplier).toBeGreaterThan(1);
  });

  it("retryable codes are safe-only by default", () => {
    const p = RetryPolicy.none();
    expect(p.retryableCodes).toContain("UNAVAILABLE");
    expect(p.retryableCodes).not.toContain("INTERNAL");
    expect(p.retryableCodes).not.toContain("ABORTED");
  });

  it("fixed uses a fixed backoff", () => {
    const p = RetryPolicy.fixed(3, 0.5);
    expect(p.backoff).toBe(Backoff.FIXED);
    expect(p.initialBackoffSecs).toBe(0.5);
  });
});

describe("CircuitBreaker", () => {
  it("default trip threshold is one half", () => {
    expect(CircuitBreaker.default().tripThreshold).toBeCloseTo(0.5);
  });

  it("aggressive trips faster than conservative", () => {
    const a = CircuitBreaker.aggressive();
    const c = CircuitBreaker.conservative();
    expect(a.minRequests).toBeLessThan(c.minRequests);
    expect(a.tripThreshold).toBeLessThan(c.tripThreshold);
  });
});
