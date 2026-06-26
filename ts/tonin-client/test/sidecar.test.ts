// Sidecar-style end-to-end test: client → tonin-proxy → upstream.
//
// This is how non-Rust services actually call peers in tonin — the app dials
// its per-pod `tonin-proxy` sidecar (localhost) over plain gRPC, and the proxy
// forwards to the upstream while passing auth / trace headers through verbatim
// (coalescing, caching, retry and circuit-breaking happen there). This test
// stands up the real `tonin-proxy` binary as the intermediary and asserts the
// client's propagated headers survive the sidecar hop.
//
// It auto-skips when the proxy binary isn't built, so plain `npm test` stays
// hermetic (node-only). Build it with `cargo build -p tonin-proxy` (or
// `--profile proxy-release`), or point `TONIN_PROXY_BIN` at the binary. CI's
// `ts-client-sidecar` job builds it and runs this suite.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { connect, createServer } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import * as grpc from "@grpc/grpc-js";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { AuthCtx } from "../src/index.js";
import { injectTraceparent } from "../src/propagate.js";
import { echo, startEchoServer } from "./_grpc-helpers.js";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../.."); // ts/tonin-client/test → repo root

function resolveProxyBin(): string | null {
  const fromEnv = process.env.TONIN_PROXY_BIN;
  if (fromEnv && existsSync(fromEnv)) return fromEnv;
  for (const profile of ["proxy-release", "debug", "release"]) {
    const candidate = resolve(repoRoot, "target", profile, "tonin-proxy");
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

const proxyBin = resolveProxyBin();

/** Grab a free TCP port (bind to :0, read it back, release it). */
async function freePort(): Promise<number> {
  return new Promise((res, rej) => {
    const s = createServer();
    s.once("error", rej);
    s.listen(0, "127.0.0.1", () => {
      const addr = s.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      s.close(() => res(port));
    });
  });
}

/** Poll until `port` accepts a TCP connection, or throw after `timeoutMs`. */
async function waitForPort(port: number, timeoutMs = 8000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const open = await new Promise<boolean>((res) => {
      const sock = connect(port, "127.0.0.1");
      sock.once("connect", () => {
        sock.destroy();
        res(true);
      });
      sock.once("error", () => {
        sock.destroy();
        res(false);
      });
    });
    if (open) return;
    if (Date.now() > deadline) throw new Error(`tonin-proxy port ${port} not open after ${timeoutMs}ms`);
    await new Promise((r) => setTimeout(r, 150));
  }
}

describe.skipIf(!proxyBin)("tonin-client through the tonin-proxy sidecar", () => {
  let upstream: grpc.Server;
  let proxy: ChildProcess;
  let client: grpc.Client;

  beforeAll(async () => {
    const started = await startEchoServer();
    upstream = started.server;

    const proxyPort = await freePort();
    proxy = spawn(proxyBin as string, [], {
      env: {
        ...process.env,
        TONIN_PROXY_UPSTREAM: `http://127.0.0.1:${started.port}`,
        TONIN_PROXY_PORT: String(proxyPort),
        TONIN_PROXY_LOG: "warn",
      },
      stdio: "inherit",
    });
    proxy.on("exit", (code) => {
      if (code && code !== 0) console.error(`tonin-proxy exited with code ${code}`);
    });

    await waitForPort(proxyPort);
    client = new grpc.Client(`127.0.0.1:${proxyPort}`, grpc.credentials.createInsecure());
  }, 30_000);

  afterAll(async () => {
    client?.close();
    proxy?.kill("SIGTERM");
    if (upstream) await new Promise<void>((resolve) => upstream.tryShutdown(() => resolve()));
  });

  it("forwards AuthCtx.propagate's bearer token to the upstream", async () => {
    const md = new grpc.Metadata();
    AuthCtx.fromBearer("test-token").propagate(md);
    const seen = await echo(client, md, "ping-auth");
    expect(seen.authorization).toEqual(["Bearer test-token"]);
    expect(seen.body).toBe("ping-auth");
  });

  it("forwards W3C trace headers to the upstream", async () => {
    const md = new grpc.Metadata();
    const tp = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
    injectTraceparent(md, tp);
    const seen = await echo(client, md, "ping-trace");
    expect(seen.traceparent).toEqual([tp]);
  });
});
