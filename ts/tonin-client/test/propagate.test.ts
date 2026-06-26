// Unit tests for `propagate` — mirrors
// python/tonin-client/tests/test_propagate.py and
// crates/tonin-client/src/propagate.rs tests.

import { describe, expect, it, vi } from "vitest";

import {
  formatGrpcTimeout,
  injectDeadline,
  injectTraceparent,
  injectTracestate,
} from "../src/propagate.js";

const VALID_TRACEPARENT = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

describe("injectTraceparent", () => {
  it("injects into list metadata", () => {
    const md: Array<[string, string]> = [];
    expect(injectTraceparent(md, VALID_TRACEPARENT)).toBe(true);
    expect(md).toContainEqual(["traceparent", VALID_TRACEPARENT]);
  });

  it("injects into record metadata", () => {
    const md: Record<string, string> = {};
    expect(injectTraceparent(md, VALID_TRACEPARENT)).toBe(true);
    expect(md.traceparent).toBe(VALID_TRACEPARENT);
  });

  it("drops a malformed value with a warning", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const md: Array<[string, string]> = [];
    expect(injectTraceparent(md, "bad-value")).toBe(false);
    expect(md).toEqual([]);
    expect(warn).toHaveBeenCalledOnce();
    warn.mockRestore();
  });

  it("rejects a wrong version", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const md: Array<[string, string]> = [];
    const bad = "01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
    expect(injectTraceparent(md, bad)).toBe(false);
    expect(md).toEqual([]);
    warn.mockRestore();
  });

  it("rejects uppercase hex (W3C requires lowercase)", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const md: Array<[string, string]> = [];
    const upper = "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01";
    expect(injectTraceparent(md, upper)).toBe(false);
    warn.mockRestore();
  });
});

describe("injectTracestate", () => {
  it("injects into list metadata", () => {
    const md: Array<[string, string]> = [];
    expect(injectTracestate(md, "vendor=value")).toBe(true);
    expect(md).toContainEqual(["tracestate", "vendor=value"]);
  });

  it("is a no-op for an empty value", () => {
    const md: Array<[string, string]> = [];
    expect(injectTracestate(md, "")).toBe(false);
    expect(md).toEqual([]);
  });
});

describe("formatGrpcTimeout", () => {
  it("picks the coarsest lossless unit", () => {
    expect(formatGrpcTimeout(3600)).toBe("1H");
    expect(formatGrpcTimeout(120)).toBe("2M");
    expect(formatGrpcTimeout(5)).toBe("5S");
    expect(formatGrpcTimeout(0.45)).toBe("450m");
    expect(formatGrpcTimeout(0.00075)).toBe("750u");
    expect(formatGrpcTimeout(0.000000123)).toBe("123n");
  });

  it("falls to a finer unit at a sub-unit boundary", () => {
    expect(formatGrpcTimeout(1.5)).toBe("1500m");
  });
});

describe("injectDeadline", () => {
  it("writes grpc-timeout for a future deadline (list)", () => {
    const md: Array<[string, string]> = [];
    expect(injectDeadline(md, performance.now() + 5000)).toBe(true);
    expect(md.some(([k]) => k === "grpc-timeout")).toBe(true);
  });

  it("writes grpc-timeout for a future deadline (record)", () => {
    const md: Record<string, string> = {};
    expect(injectDeadline(md, performance.now() + 5000)).toBe(true);
    expect(md["grpc-timeout"]).toBeDefined();
  });

  it("returns false for a deadline already passed", () => {
    const md: Array<[string, string]> = [];
    expect(injectDeadline(md, performance.now() - 1000)).toBe(false);
    expect(md).toEqual([]);
  });

  it("emits a valid grpc-timeout format", () => {
    const md: Record<string, string> = {};
    injectDeadline(md, performance.now() + 2000);
    const value = md["grpc-timeout"];
    expect(value).toBeDefined();
    const v = value as string;
    expect("HMSmun").toContain(v[v.length - 1]);
    expect(/^\d+$/.test(v.slice(0, -1))).toBe(true);
  });
});
