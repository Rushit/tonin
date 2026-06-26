// Outbound gRPC metadata helpers, shared by `auth` and `propagate`.
//
// Kept dependency-free on purpose: `MetadataLike` is the *structural*
// shape of a `@grpc/grpc-js` `Metadata` object (anything with a
// `.set(key, value)` method), so a real `Metadata` instance is assignable
// without this package depending on `@grpc/grpc-js`.

/**
 * Minimal structural shape of a `@grpc/grpc-js` `Metadata` object: anything
 * exposing a `.set(key, value)` method. A real `Metadata` instance satisfies
 * this, so callers can pass one directly — no dependency required.
 */
export interface MetadataLike {
  set(key: string, value: string): void;
}

/**
 * The outbound-metadata shapes the injection helpers accept:
 *
 * - a `@grpc/grpc-js` `Metadata` (or anything with `.set`) — mutated via `.set`
 * - a plain headers object (`Record<string, string>`) — key assigned
 * - a list of `[key, value]` tuples — appended (mirrors Python's `grpc.aio` form)
 */
export type OutboundMetadata = MetadataLike | Record<string, string> | Array<[string, string]>;

/** Inject a single header into any supported metadata shape. */
export function injectHeader(metadata: OutboundMetadata, key: string, value: string): void {
  if (Array.isArray(metadata)) {
    metadata.push([key, value]);
  } else if (typeof (metadata as MetadataLike).set === "function") {
    (metadata as MetadataLike).set(key, value);
  } else {
    (metadata as Record<string, string>)[key] = value;
  }
}
