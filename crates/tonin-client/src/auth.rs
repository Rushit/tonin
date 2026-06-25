//! Auth types shared between server and client.
//!
//! These types are the contract between:
//! - the inbound `AuthLayer` (in `tonin-core::auth::layer`) which
//!   produces an `AuthCtx` from a verified token, and
//! - the outbound client SDK which copies the bearer token onto
//!   downstream requests via [`AuthCtx::propagate`].
//!
//! Custom verifiers (Okta, Cognito, API keys, cookies) are implemented
//! on the server side via the `TokenVerifier` trait — but they all
//! return the same `AuthCtx` defined here.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tonic::{Request, Status};

/// A token as extracted from a request, before verification.
///
/// The framework doesn't assume JWT — the value could be a session ID,
/// API key, or anything else a server-side `TokenVerifier` knows how to
/// handle.
#[derive(Clone, Debug)]
pub struct RawToken {
    pub value: String,
    /// Hint for verifiers that handle multiple token formats.
    /// Conventions: `"bearer-jwt"`, `"api-key"`, `"session-cookie"`,
    /// `"basic-auth"`, etc.
    pub kind: &'static str,
}

/// Identity + claims for the current request. The single concrete type
/// that flows through the framework — outbound clients accept this, the
/// server-side auth layer produces it. Custom claims live in
/// [`Self::extra`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthCtx {
    /// `sub` claim. User ID for users, service ID for services.
    pub subject: String,
    pub issuer: String,
    pub audience: String,
    pub scopes: Vec<String>,
    pub kind: PrincipalKind,
    /// The verbatim token. Used by [`AuthCtx::propagate`] for outbound calls.
    pub raw_token: String,
    /// Unix-seconds expiry. f64 to stay JSON-compatible with the
    /// Python and TS sides (which use `number`). `0.0` means "no
    /// expiry recorded" (e.g. an anonymous context). Use
    /// [`AuthCtx::expires_at_systime`] / [`AuthCtx::set_expires_at_systime`]
    /// when interop with `std::time::SystemTime` is convenient.
    pub expires_at: f64,
    /// Claims not mapped to typed fields. Verifiers populate this with
    /// anything custom (e.g. tenant_id, role, agent_on_behalf_of).
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    User,
    Service,
    Agent,
    /// No auth was attempted (service is in opt-out mode).
    Anonymous,
}

impl AuthCtx {
    /// Returns an empty `AuthCtx` for opt-out / no-auth flows.
    pub fn anonymous() -> Self {
        Self {
            subject: String::new(),
            issuer: String::new(),
            audience: String::new(),
            scopes: Vec::new(),
            kind: PrincipalKind::Anonymous,
            raw_token: String::new(),
            expires_at: 0.0,
            extra: HashMap::new(),
        }
    }

    /// Wrap a bearer token without verification. For client-side code
    /// that already has a token (e.g., from a login flow) and wants to
    /// hand it to the framework's outbound propagation.
    pub fn from_bearer(token: impl Into<String>) -> Self {
        let token = token.into();
        Self {
            raw_token: token,
            kind: PrincipalKind::User,
            ..Self::anonymous()
        }
    }

    /// Pull `AuthCtx` from a tonic request's extensions, populated by
    /// the inbound auth layer. Returns [`Self::anonymous`] if no layer
    /// ran.
    pub fn from<T>(req: &Request<T>) -> Self {
        req.extensions()
            .get::<AuthCtx>()
            .cloned()
            .unwrap_or_else(Self::anonymous)
    }

    /// Copy the bearer token onto an outbound request so the caller's
    /// identity rides along to the next service.
    pub fn propagate<T>(&self, req: &mut Request<T>) {
        if self.raw_token.is_empty() {
            return;
        }
        if let Ok(value) = format!("Bearer {}", self.raw_token).parse() {
            req.metadata_mut().insert("authorization", value);
        }
    }

    /// Authorize a single scope. Returns `PermissionDenied` if missing.
    /// Convenient for `Status` returns from handlers.
    #[allow(clippy::result_large_err)] // tonic::Status is the canonical error type for gRPC handlers
    pub fn require_scope(&self, scope: &str) -> Result<(), Status> {
        if self.scopes.iter().any(|s| s == scope) {
            Ok(())
        } else {
            Err(AuthError::InsufficientScope {
                required: scope.into(),
            }
            .into())
        }
    }

    pub fn is_anonymous(&self) -> bool {
        matches!(self.kind, PrincipalKind::Anonymous)
    }

    /// Returns `true` if the token has passed its recorded expiry.
    ///
    /// `false` for an anonymous context (`expires_at == 0.0`) — an
    /// unset expiry is treated as "no expiry recorded", not as expired.
    pub fn is_expired(&self) -> bool {
        if self.expires_at <= 0.0 {
            return false;
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64() > self.expires_at)
            .unwrap_or(false)
    }

    /// Convert `expires_at` (unix seconds) into a `SystemTime`. Returns
    /// `UNIX_EPOCH` for an anonymous / unset context (`expires_at == 0.0`).
    pub fn expires_at_systime(&self) -> SystemTime {
        if self.expires_at <= 0.0 {
            UNIX_EPOCH
        } else {
            UNIX_EPOCH + std::time::Duration::from_secs_f64(self.expires_at)
        }
    }

    /// Set `expires_at` from a `SystemTime`. Convenience for verifiers
    /// that already hold a `SystemTime` (e.g. JWT `iat + max_age`).
    pub fn set_expires_at_systime(&mut self, t: SystemTime) {
        self.expires_at = t
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("no token in request")]
    MissingToken,
    #[error("token signature invalid")]
    Signature,
    #[error("token expired")]
    Expired,
    #[error("audience mismatch: expected {expected}, got {got}")]
    Audience { expected: String, got: String },
    #[error("issuer mismatch: expected {expected}, got {got}")]
    Issuer { expected: String, got: String },
    #[error("token verification failed: {0}")]
    Verification(String),
    #[error("insufficient scope: required {required}")]
    InsufficientScope { required: String },
    #[error("configuration error: {0}")]
    Config(String),
    #[error("transport error contacting auth backend: {0}")]
    Transport(String),
}

impl From<AuthError> for Status {
    fn from(e: AuthError) -> Status {
        match e {
            AuthError::InsufficientScope { .. } => Status::permission_denied(e.to_string()),
            AuthError::Config(_) | AuthError::Transport(_) => Status::internal(e.to_string()),
            _ => Status::unauthenticated(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_authctx_is_anonymous() {
        let a = AuthCtx::anonymous();
        assert!(a.is_anonymous());
        assert_eq!(a.kind, PrincipalKind::Anonymous);
    }

    #[test]
    fn from_bearer_carries_token() {
        let a = AuthCtx::from_bearer("abc.def.ghi");
        assert_eq!(a.raw_token, "abc.def.ghi");
        assert_eq!(a.kind, PrincipalKind::User);
    }

    #[test]
    fn propagate_writes_authorization_header() {
        let a = AuthCtx::from_bearer("abc.def.ghi");
        let mut req = Request::new(());
        a.propagate(&mut req);
        let v = req.metadata().get("authorization").unwrap();
        assert_eq!(v.to_str().unwrap(), "Bearer abc.def.ghi");
    }

    #[test]
    fn propagate_anonymous_is_noop() {
        let a = AuthCtx::anonymous();
        let mut req = Request::new(());
        a.propagate(&mut req);
        assert!(req.metadata().get("authorization").is_none());
    }

    #[test]
    fn require_scope_ok_when_present() {
        let mut a = AuthCtx::anonymous();
        a.scopes = vec!["read:billing".into()];
        assert!(a.require_scope("read:billing").is_ok());
    }

    #[test]
    fn require_scope_err_when_missing() {
        let a = AuthCtx::anonymous();
        let err = a.require_scope("admin").unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn is_expired_false_for_anonymous() {
        assert!(!AuthCtx::anonymous().is_expired());
    }

    #[test]
    fn is_expired_false_when_future() {
        let mut ctx = AuthCtx::anonymous();
        // expires_at far in the future
        ctx.expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            + 3600.0;
        assert!(!ctx.is_expired());
    }

    #[test]
    fn is_expired_true_when_past() {
        let mut ctx = AuthCtx::anonymous();
        ctx.expires_at = 1.0; // 1970-01-01 — definitely in the past
        assert!(ctx.is_expired());
    }

    #[test]
    fn auth_error_maps_to_correct_status() {
        let s: Status = AuthError::Signature.into();
        assert_eq!(s.code(), tonic::Code::Unauthenticated);

        let s: Status = AuthError::InsufficientScope {
            required: "admin".into(),
        }
        .into();
        assert_eq!(s.code(), tonic::Code::PermissionDenied);

        let s: Status = AuthError::Config("missing env".into()).into();
        assert_eq!(s.code(), tonic::Code::Internal);
    }

    /// Lock in the JSON wire shape across language boundaries.
    ///
    /// If this test breaks, the Python/TS sides need an update too —
    /// run `cargo run --bin gen-shared-types` and re-check
    /// `python/tonin-client/tests/test_wire_compat.py`.
    #[test]
    fn authctx_json_shape_is_stable_for_polyglot_consumers() {
        let mut ctx = AuthCtx::anonymous();
        ctx.subject = "alice".into();
        ctx.issuer = "https://issuer.example".into();
        ctx.audience = "my-svc".into();
        ctx.scopes = vec!["read:billing".into(), "write:billing".into()];
        ctx.kind = PrincipalKind::User;
        ctx.raw_token = "abc.def.ghi".into();
        ctx.expires_at = 1_735_689_600.0;
        ctx.extra
            .insert("tenant_id".into(), serde_json::json!("acme"));

        let v = serde_json::to_value(&ctx).unwrap();
        // Field-name presence (snake_case, no rename).
        for f in [
            "subject",
            "issuer",
            "audience",
            "scopes",
            "kind",
            "raw_token",
            "expires_at",
            "extra",
        ] {
            assert!(
                v.get(f).is_some(),
                "missing field `{f}` in serialized AuthCtx JSON shape"
            );
        }
        // Critical contract: expires_at is a JSON number (not a struct).
        assert!(v["expires_at"].is_number());
        // Critical contract: kind serializes as lowercase string.
        assert_eq!(v["kind"], serde_json::json!("user"));
    }
}
