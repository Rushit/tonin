// Cross-language wire compatibility — mirrors
// python/tonin-client/tests/test_wire_compat.py.
//
// Rust's serde produces a specific snake_case JSON shape for `AuthCtx`. The TS
// side must round-trip it through `AuthCtx.fromWire` / `AuthCtx.toWire` without
// losing or renaming fields. If the Rust struct's serde shape changes, this
// test (together with generated-match) catches the break before it ships.

import { describe, expect, it } from "vitest";

import { AuthCtx, PrincipalKind } from "../src/index.js";
import type { AuthCtxWire } from "../src/index.js";

// Sample JSON matching `serde_json::to_value(&AuthCtx { ... })` output:
// snake_case field names, `kind` lowercase (#[serde(rename_all = "lowercase")]).
const SAMPLE_AUTHCTX_WIRE: AuthCtxWire = {
  subject: "alice",
  issuer: "https://issuer.example",
  audience: "my-svc",
  scopes: ["read:billing", "write:billing"],
  kind: PrincipalKind.USER,
  raw_token: "abc.def.ghi",
  expires_at: 1735689600,
  extra: { tenant_id: "acme" },
};

describe("wire compatibility", () => {
  it("fromWire accepts the serde snake_case shape", () => {
    const ctx = AuthCtx.fromWire(SAMPLE_AUTHCTX_WIRE);
    expect(ctx.subject).toBe("alice");
    expect(ctx.kind).toBe(PrincipalKind.USER);
    expect(ctx.scopes).toEqual(["read:billing", "write:billing"]);
    expect(ctx.rawToken).toBe("abc.def.ghi");
    expect(ctx.expiresAt).toBe(1735689600);
  });

  it("toWire round-trips back to snake_case", () => {
    const wire = AuthCtx.fromWire(SAMPLE_AUTHCTX_WIRE).toWire();
    expect(wire).toEqual(SAMPLE_AUTHCTX_WIRE);
    expect(Object.keys(wire)).toContain("raw_token");
    expect(Object.keys(wire)).toContain("expires_at");
  });

  it("PrincipalKind wire values are lowercase", () => {
    for (const v of Object.values(PrincipalKind)) {
      expect(v).toBe(v.toLowerCase());
    }
  });

  it("toWire output is JSON-serializable with snake_case keys", () => {
    const ctx = new AuthCtx({ subject: "bob", kind: PrincipalKind.SERVICE, rawToken: "tok" });
    const decoded = JSON.parse(JSON.stringify(ctx.toWire()));
    expect(decoded.subject).toBe("bob");
    expect(decoded.kind).toBe("service");
    expect(decoded.raw_token).toBe("tok");
  });
});
