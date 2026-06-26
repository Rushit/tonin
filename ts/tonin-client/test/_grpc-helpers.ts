// Shared gRPC test rig for the integration tests (e2e + sidecar).
//
// A tiny "echo" unary service defined directly against `@grpc/grpc-js`'s
// low-level API — no proto compilation needed. The handler reflects the
// request metadata (and body) back to the caller, so a test can assert exactly
// which headers arrived at the upstream. `@grpc/grpc-js` is a dev-only
// dependency; the package itself ships zero runtime deps.
//
// Not a `*.test.ts` file, so vitest does not run it as a suite.

import * as grpc from "@grpc/grpc-js";

export const ECHO_PATH = "/tonin.test.Echo/Echo";

const passthrough = (b: Buffer): Buffer => b;

/** What the upstream echo handler saw on an incoming call. */
export interface SeenMetadata {
  authorization: string[];
  traceparent: string[];
  tracestate: string[];
  body: string;
}

const ECHO_SERVICE: grpc.ServiceDefinition = {
  echo: {
    path: ECHO_PATH,
    requestStream: false,
    responseStream: false,
    requestSerialize: passthrough,
    requestDeserialize: passthrough,
    responseSerialize: passthrough,
    responseDeserialize: passthrough,
  },
};

/** Start an echo gRPC server that reflects request metadata + body back. */
export async function startEchoServer(): Promise<{ server: grpc.Server; port: number }> {
  const server = new grpc.Server();
  server.addService(ECHO_SERVICE, {
    echo: (call: grpc.ServerUnaryCall<Buffer, Buffer>, callback: grpc.sendUnaryData<Buffer>) => {
      const md = call.metadata;
      const seen: SeenMetadata = {
        authorization: md.get("authorization").map(String),
        traceparent: md.get("traceparent").map(String),
        tracestate: md.get("tracestate").map(String),
        body: call.request.toString(),
      };
      callback(null, Buffer.from(JSON.stringify(seen)));
    },
  });

  const port = await new Promise<number>((resolve, reject) => {
    server.bindAsync("127.0.0.1:0", grpc.ServerCredentials.createInsecure(), (err, p) => {
      if (err) reject(err);
      else resolve(p);
    });
  });
  return { server, port };
}

/** Make one unary echo call; resolves with what the upstream server saw. */
export function echo(client: grpc.Client, metadata: grpc.Metadata, body = ""): Promise<SeenMetadata> {
  return new Promise((resolve, reject) => {
    client.makeUnaryRequest<Buffer, Buffer>(
      ECHO_PATH,
      passthrough,
      passthrough,
      Buffer.from(body),
      metadata,
      (err, res) => {
        if (err) reject(err);
        else resolve(JSON.parse((res as Buffer).toString()) as SeenMetadata);
      },
    );
  });
}
