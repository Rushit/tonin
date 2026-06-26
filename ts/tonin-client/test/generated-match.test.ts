// Drift detection: the hand-written camelCase types in auth.ts / retry.ts /
// breaker.ts must stay in sync with the codegen output in _generated.ts (the
// snake_case mirror of the Rust source of truth).
//
// This is the TypeScript analogue of python/tonin-client/tests/
// test_generated_match.py. TS interfaces are erased at runtime, so we can't
// introspect `_generated.ts` field names directly. Instead we build *typed
// sample literals* against the generated interfaces: the TS compiler forces
// each literal to match its interface exactly, so if Rust adds a field and
// `cargo run --bin gen-shared-types` regenerates _generated.ts, the literal
// below stops type-checking — and the runtime key comparison fails — until the
// hand-written type is updated to match.
//
// If this fails: run `cargo run --bin gen-shared-types --features cli` to
// refresh _generated.ts, then reconcile the hand-written types in
// auth.ts / retry.ts / breaker.ts.

import { describe, expect, it } from "vitest";

import * as Gen from "../src/_generated.js";
import { AuthCtx, PrincipalKind } from "../src/auth.js";
import { CircuitBreaker } from "../src/breaker.js";
import { Backoff, RetryPolicy } from "../src/retry.js";

const camelToSnake = (s: string): string => s.replace(/[A-Z]/g, (m) => `_${m.toLowerCase()}`);

// Typed sample literals — must satisfy the generated interfaces exactly.
const GEN_AUTHCTX: Gen.AuthCtx = {
  subject: "",
  issuer: "",
  audience: "",
  scopes: [],
  kind: Gen.PrincipalKind.ANONYMOUS,
  raw_token: "",
  expires_at: 0,
  extra: {},
};
const GEN_RAWTOKEN: Gen.RawToken = { value: "", kind: "" };
const GEN_RETRY: Gen.RetryPolicy = {
  max_attempts: 0,
  initial_backoff_secs: 0,
  backoff: Gen.Backoff.FIXED,
  multiplier: 0,
  max_backoff_secs: 0,
};
const GEN_BREAKER: Gen.CircuitBreaker = {
  window_secs: 0,
  trip_threshold: 0,
  min_requests: 0,
  reset_after_secs: 0,
  half_open_probes: 0,
};

const keys = (obj: object): Set<string> => new Set(Object.keys(obj));
const handKeysAsSnake = (obj: object): Set<string> =>
  new Set(Object.keys(obj).map(camelToSnake));

describe("generated-match (drift gate)", () => {
  it("AuthCtx fields match the generated shape", () => {
    expect(handKeysAsSnake(new AuthCtx())).toEqual(keys(GEN_AUTHCTX));
  });

  it("RawToken fields match the generated shape", () => {
    // Hand-written RawToken is interface-only; this typed literal mirrors it.
    const handRawToken: import("../src/auth.js").RawToken = { value: "", kind: "" };
    expect(handKeysAsSnake(handRawToken)).toEqual(keys(GEN_RAWTOKEN));
  });

  it("PrincipalKind variants match the generated wire values", () => {
    expect(new Set(Object.values(PrincipalKind))).toEqual(new Set(Object.values(Gen.PrincipalKind)));
  });

  it("Backoff variants match the generated wire values", () => {
    expect(new Set(Object.values(Backoff))).toEqual(new Set(Object.values(Gen.Backoff)));
  });

  it("RetryPolicy is a superset of the generated shape (extra retryableCodes allowed)", () => {
    const hand = handKeysAsSnake(RetryPolicy.none());
    for (const k of keys(GEN_RETRY)) {
      expect(hand.has(k)).toBe(true);
    }
  });

  it("CircuitBreaker is a superset of the generated shape (extra failureCodes allowed)", () => {
    const hand = handKeysAsSnake(CircuitBreaker.default());
    for (const k of keys(GEN_BREAKER)) {
      expect(hand.has(k)).toBe(true);
    }
  });
});
