// End-to-end test over a real grpc-js wire (direct, no sidecar).
//
// The TS analogue of python/.../server/tests/test_e2e.py: boot a real gRPC
// server on a random port and hit it with a real client. Proves the client
// primitives integrate with the genuine `@grpc/grpc-js` `Metadata` type — in
// particular that `AuthCtx.propagate` (structural typing, no grpc-js runtime
// dep) flows the bearer token over the actual wire.

import * as grpc from "@grpc/grpc-js";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { AuthCtx } from "../src/index.js";
import { injectTraceparent, injectTracestate } from "../src/propagate.js";
import { echo, startEchoServer } from "./_grpc-helpers.js";

const TRACEPARENT = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

let server: grpc.Server;
let client: grpc.Client;

beforeAll(async () => {
  const started = await startEchoServer();
  server = started.server;
  client = new grpc.Client(`127.0.0.1:${started.port}`, grpc.credentials.createInsecure());
});

afterAll(async () => {
  client.close();
  await new Promise<void>((resolve) => server.tryShutdown(() => resolve()));
});

describe("tonin-client over real grpc-js (direct)", () => {
  it("AuthCtx.propagate flows the bearer token through real gRPC metadata", async () => {
    const md = new grpc.Metadata();
    AuthCtx.fromBearer("test-token").propagate(md);
    const seen = await echo(client, md);
    expect(seen.authorization).toEqual(["Bearer test-token"]);
  });

  it("an anonymous AuthCtx sends no authorization header", async () => {
    const md = new grpc.Metadata();
    AuthCtx.anonymous().propagate(md);
    const seen = await echo(client, md);
    expect(seen.authorization).toEqual([]);
  });

  it("trace headers flow through real gRPC metadata", async () => {
    const md = new grpc.Metadata();
    injectTraceparent(md, TRACEPARENT);
    injectTracestate(md, "vendor=abc");
    const seen = await echo(client, md);
    expect(seen.traceparent).toEqual([TRACEPARENT]);
    expect(seen.tracestate).toEqual(["vendor=abc"]);
  });
});
