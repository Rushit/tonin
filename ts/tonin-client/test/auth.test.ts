// Unit tests for `auth` — mirrors python/tonin-client/tests/test_auth.py and
// crates/tonin-client auth tests.

import { describe, expect, it } from "vitest";

import { AuthCtx, AuthError, GrpcStatus, PrincipalKind } from "../src/index.js";

describe("AuthCtx", () => {
  it("anonymous is anonymous", () => {
    const a = AuthCtx.anonymous();
    expect(a.isAnonymous()).toBe(true);
    expect(a.kind).toBe(PrincipalKind.ANONYMOUS);
  });

  it("fromBearer carries the token", () => {
    const a = AuthCtx.fromBearer("abc.def.ghi");
    expect(a.rawToken).toBe("abc.def.ghi");
    expect(a.kind).toBe(PrincipalKind.USER);
  });

  it("propagate into list metadata", () => {
    const md: Array<[string, string]> = [];
    AuthCtx.fromBearer("abc.def.ghi").propagate(md);
    expect(md).toContainEqual(["authorization", "Bearer abc.def.ghi"]);
  });

  it("propagate into record metadata", () => {
    const md: Record<string, string> = {};
    AuthCtx.fromBearer("abc.def.ghi").propagate(md);
    expect(md.authorization).toBe("Bearer abc.def.ghi");
  });

  it("propagate into a grpc-js-like Metadata (.set)", () => {
    const calls: Array<[string, string]> = [];
    const metadata = { set: (k: string, v: string) => calls.push([k, v]) };
    AuthCtx.fromBearer("tok").propagate(metadata);
    expect(calls).toContainEqual(["authorization", "Bearer tok"]);
  });

  it("propagate is a no-op for an anonymous ctx", () => {
    const md: Array<[string, string]> = [];
    AuthCtx.anonymous().propagate(md);
    expect(md).toEqual([]);
  });

  it("requireScope succeeds when present", () => {
    const a = new AuthCtx({ scopes: ["read:billing"] });
    expect(() => a.requireScope("read:billing")).not.toThrow();
  });

  it("requireScope throws when missing", () => {
    const a = AuthCtx.anonymous();
    let caught: unknown;
    try {
      a.requireScope("admin");
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(AuthError);
    expect((caught as AuthError).code).toBe("insufficient_scope");
    expect((caught as AuthError).requiredScope).toBe("admin");
  });

  it("isExpired is false for anonymous", () => {
    expect(AuthCtx.anonymous().isExpired()).toBe(false);
  });

  it("isExpired is false when expiry is in the future", () => {
    const a = new AuthCtx({ expiresAt: Date.now() / 1000 + 3600 });
    expect(a.isExpired()).toBe(false);
  });

  it("isExpired is true when expiry is in the past", () => {
    const a = new AuthCtx({ expiresAt: 1 }); // 1970 — definitely past
    expect(a.isExpired()).toBe(true);
  });
});

describe("AuthError", () => {
  it("maps to the correct gRPC status", () => {
    expect(AuthError.signature().toGrpcStatusCode()).toBe(GrpcStatus.UNAUTHENTICATED);
    expect(AuthError.insufficientScope("admin").toGrpcStatusCode()).toBe(
      GrpcStatus.PERMISSION_DENIED,
    );
    expect(AuthError.config("missing env").toGrpcStatusCode()).toBe(GrpcStatus.INTERNAL);
  });
});
