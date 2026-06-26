// Auth types shared between the TypeScript server and client sides, and
// byte-for-byte compatible with the Rust (`tonin-client::auth`) and Python
// (`tonin_client.auth`) peers across the polyglot mesh.
//
// `AuthCtx` is the single concrete identity type that flows through the
// framework. The inbound interceptor produces one from a verified token;
// outbound client code copies the bearer token onto downstream requests via
// `AuthCtx.propagate`.
//
// Field names here are idiomatic camelCase. The cross-language *wire* shape is
// snake_case (matching Rust's serde output); use `toWire()` / `fromWire()` to
// cross that boundary.

import { injectHeader, type OutboundMetadata } from "./_meta.js";

/** Who is making the call. Lowercase values match the Rust/Python enums. */
export enum PrincipalKind {
  USER = "user",
  SERVICE = "service",
  AGENT = "agent",
  ANONYMOUS = "anonymous",
}

/** A token as extracted from a request, before verification. */
export interface RawToken {
  value: string;
  /** Conventions: `"bearer-jwt"`, `"api-key"`, `"session-cookie"`, `"basic-auth"`. */
  kind: string;
}

/**
 * Canonical gRPC status codes. The numeric values match `@grpc/grpc-js`'s
 * `status` enum, so a consumer can compare `AuthError.toGrpcStatusCode()`
 * against grpc-js codes directly — without this package depending on grpc-js.
 */
export enum GrpcStatus {
  OK = 0,
  CANCELLED = 1,
  UNKNOWN = 2,
  INVALID_ARGUMENT = 3,
  DEADLINE_EXCEEDED = 4,
  NOT_FOUND = 5,
  ALREADY_EXISTS = 6,
  PERMISSION_DENIED = 7,
  RESOURCE_EXHAUSTED = 8,
  FAILED_PRECONDITION = 9,
  ABORTED = 10,
  OUT_OF_RANGE = 11,
  UNIMPLEMENTED = 12,
  INTERNAL = 13,
  UNAVAILABLE = 14,
  DATA_LOSS = 15,
  UNAUTHENTICATED = 16,
}

/** Partial initializer for {@link AuthCtx}. */
export interface AuthCtxInit {
  subject?: string;
  issuer?: string;
  audience?: string;
  scopes?: string[];
  kind?: PrincipalKind;
  rawToken?: string;
  expiresAt?: number;
  extra?: Record<string, unknown>;
}

/**
 * The cross-language wire shape of {@link AuthCtx} — snake_case field names
 * matching Rust's serde output and Python's dataclass. Use {@link AuthCtx.toWire}
 * / {@link AuthCtx.fromWire} to convert to and from this shape.
 */
export interface AuthCtxWire {
  subject: string;
  issuer: string;
  audience: string;
  scopes: string[];
  kind: PrincipalKind;
  raw_token: string;
  expires_at: number;
  extra: Record<string, unknown>;
}

/**
 * Identity + claims for the current request. The single concrete type that
 * crosses language boundaries — peer Rust/Python services produce the same
 * shape from their own interceptors.
 */
export class AuthCtx {
  subject = "";
  issuer = "";
  audience = "";
  scopes: string[] = [];
  kind: PrincipalKind = PrincipalKind.ANONYMOUS;
  /** Verbatim token; used by {@link propagate} on outbound calls. */
  rawToken = "";
  /** Unix-seconds expiry of the token (`0` = no expiry recorded). */
  expiresAt = 0;
  /** Custom claims not mapped to typed fields. */
  extra: Record<string, unknown> = {};

  constructor(init: AuthCtxInit = {}) {
    Object.assign(this, init);
  }

  /** Empty `AuthCtx` for opt-out / no-auth flows. */
  static anonymous(): AuthCtx {
    return new AuthCtx();
  }

  /**
   * Wrap a bearer token without verification. For client-side code that
   * already has a token (e.g. from a login flow) and wants to hand it to the
   * framework's outbound propagation. Server-side code receives `AuthCtx` from
   * the interceptor and should not call this.
   */
  static fromBearer(token: string): AuthCtx {
    return new AuthCtx({ rawToken: token, kind: PrincipalKind.USER });
  }

  isAnonymous(): boolean {
    return this.kind === PrincipalKind.ANONYMOUS;
  }

  /**
   * Return `true` if the token has passed its recorded expiry. `false` for an
   * unset expiry (`expiresAt <= 0`) — treated as "no expiry recorded", not as
   * expired.
   */
  isExpired(): boolean {
    if (this.expiresAt <= 0) return false;
    return Date.now() / 1000 > this.expiresAt;
  }

  /**
   * Inject the bearer token into outbound gRPC metadata as
   * `authorization: Bearer <token>`. No-op for an anonymous / token-less ctx.
   * Accepts a grpc-js `Metadata`, a plain headers object, or a `[k, v]` list.
   */
  propagate(metadata: OutboundMetadata): void {
    if (!this.rawToken) return;
    injectHeader(metadata, "authorization", `Bearer ${this.rawToken}`);
  }

  /** Authorize a single scope. Throws {@link AuthError} (PERMISSION_DENIED) if absent. */
  requireScope(scope: string): void {
    if (!this.scopes.includes(scope)) {
      throw AuthError.insufficientScope(scope);
    }
  }

  /** Convert to the snake_case cross-language wire shape. */
  toWire(): AuthCtxWire {
    return {
      subject: this.subject,
      issuer: this.issuer,
      audience: this.audience,
      scopes: [...this.scopes],
      kind: this.kind,
      raw_token: this.rawToken,
      expires_at: this.expiresAt,
      extra: { ...this.extra },
    };
  }

  /** Build an `AuthCtx` from the snake_case wire shape (e.g. Rust serde output). */
  static fromWire(w: AuthCtxWire): AuthCtx {
    return new AuthCtx({
      subject: w.subject,
      issuer: w.issuer,
      audience: w.audience,
      scopes: w.scopes,
      kind: w.kind,
      rawToken: w.raw_token,
      expiresAt: w.expires_at,
      extra: w.extra,
    });
  }
}

/**
 * Auth-related failures, categorized via {@link code} so the framework
 * interceptor can map each to the right gRPC status.
 */
export class AuthError extends Error {
  readonly code: string;
  readonly requiredScope?: string;

  constructor(code: string, message: string, requiredScope?: string) {
    super(message);
    this.name = "AuthError";
    this.code = code;
    this.requiredScope = requiredScope;
  }

  static missingToken(): AuthError {
    return new AuthError("missing_token", "no token in request");
  }
  static signature(): AuthError {
    return new AuthError("signature", "token signature invalid");
  }
  static expired(): AuthError {
    return new AuthError("expired", "token expired");
  }
  static audience(expected: string, got: string): AuthError {
    return new AuthError("audience", `audience mismatch: expected ${expected}, got ${got}`);
  }
  static issuer(expected: string, got: string): AuthError {
    return new AuthError("issuer", `issuer mismatch: expected ${expected}, got ${got}`);
  }
  static verification(detail: string): AuthError {
    return new AuthError("verification", `token verification failed: ${detail}`);
  }
  static insufficientScope(scope: string): AuthError {
    return new AuthError("insufficient_scope", `insufficient scope: required ${scope}`, scope);
  }
  static config(detail: string): AuthError {
    return new AuthError("config", `configuration error: ${detail}`);
  }
  static transport(detail: string): AuthError {
    return new AuthError("transport", `transport error contacting auth backend: ${detail}`);
  }

  /** Map this error to a gRPC status code. */
  toGrpcStatusCode(): GrpcStatus {
    if (this.code === "insufficient_scope") return GrpcStatus.PERMISSION_DENIED;
    if (this.code === "config" || this.code === "transport") return GrpcStatus.INTERNAL;
    return GrpcStatus.UNAUTHENTICATED;
  }
}
