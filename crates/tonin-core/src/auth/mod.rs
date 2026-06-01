//! Authentication and authorization layer.
//!
//! Three traits, one concrete type:
//!
//! - [`TokenExtractor`] — where the token comes from on an incoming request
//! - [`TokenVerifier`] — how the token gets validated → [`AuthCtx`]
//! - [`ServiceTokenMinter`] — how a service identity token gets minted
//!   (for background jobs / queue consumers with no user request to
//!   propagate from)
//!
//! [`AuthCtx`] is a stable struct — not generic — so the propagation
//! layer, generated client crates, and the [`CURRENT_AUTH`] task-local
//! all refer to it without generic-bounds threading. **The type itself
//! lives in `tonin-client::auth`** so client-only consumers don't
//! have to depend on the server framework; this module re-exports it.
//!
//! ## Common case (one-liner)
//!
//! ```no_run
//! # use tonin_core::Service;
//! # use tonin_core::auth::default::JwtValidator;
//! # async fn run() -> tonin_core::Result<()> {
//! let svc = Service::new("my-svc")
//!     .with_auth(JwtValidator::from_env()?);
//! # Ok(()) }
//! ```
//!
//! ## Custom verifier
//!
//! ```ignore
//! use tonin_core::auth::{TokenVerifier, AuthCtx, AuthError, RawToken};
//! use async_trait::async_trait;
//!
//! struct OktaVerifier { /* ... */ }
//!
//! #[async_trait]
//! impl TokenVerifier for OktaVerifier {
//!     async fn verify(&self, token: &RawToken) -> Result<AuthCtx, AuthError> {
//!         // ...
//!     }
//! }
//!
//! let svc = Service::new("my-svc").with_auth(OktaVerifier::from_env());
//! ```

pub mod default;
mod layer;

use std::sync::Arc;

use async_trait::async_trait;

pub use layer::AuthLayer;

// Re-export the shared client/server types from tonin-client. These
// are the contract between the inbound auth layer (here) and the
// outbound propagation in generated client SDKs (which depend on
// tonin-client but NOT on this crate).
pub use tonin_client::auth::{AuthCtx, AuthError, PrincipalKind, RawToken};

// ---------- traits ----------

/// Pulls a `RawToken` out of an incoming request's metadata.
///
/// Default: [`default::BearerHeaderExtractor`] reads `Authorization: Bearer <token>`.
///
/// The trait takes `&MetadataMap` (not `&Request<T>`) so it stays
/// dyn-safe; the [`AuthLayer`] builds a metadata view from the raw
/// `http::Request` before calling.
pub trait TokenExtractor: Send + Sync + 'static {
    fn extract(&self, metadata: &tonic::metadata::MetadataMap) -> Result<RawToken, AuthError>;
}

/// Verifies a [`RawToken`] and returns the resulting [`AuthCtx`].
///
/// Default: [`default::JwtValidator`] (signature + exp + iss + aud via JWKS).
#[async_trait]
pub trait TokenVerifier: Send + Sync + 'static {
    async fn verify(&self, token: &RawToken) -> Result<AuthCtx, AuthError>;
}

/// Mints an [`AuthCtx`] representing this service (no user). Used by
/// background jobs and queue consumers.
///
/// Default: [`default::HttpServiceTokenMinter`] POSTs to a configured
/// auth-service endpoint.
#[async_trait]
pub trait ServiceTokenMinter: Send + Sync + 'static {
    async fn mint(&self) -> Result<AuthCtx, AuthError>;
}

// ---------- composition ----------

/// Try multiple verifiers in order; first success wins. If all fail,
/// returns the last error.
pub struct ChainVerifier {
    inner: Vec<Arc<dyn TokenVerifier>>,
}

impl ChainVerifier {
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }
    #[allow(clippy::should_implement_trait)] // builder method; std::ops::Add is the wrong shape (consumes self, two args)
    pub fn add<V: TokenVerifier>(mut self, v: V) -> Self {
        self.inner.push(Arc::new(v));
        self
    }
}

impl Default for ChainVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TokenVerifier for ChainVerifier {
    async fn verify(&self, token: &RawToken) -> Result<AuthCtx, AuthError> {
        let mut last_err = AuthError::MissingToken;
        for v in &self.inner {
            match v.verify(token).await {
                Ok(ctx) => return Ok(ctx),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }
}

/// Verifier that always returns anonymous. Used by [`crate::Service::without_auth()`].
pub(crate) struct AnonymousVerifier;

#[async_trait]
impl TokenVerifier for AnonymousVerifier {
    async fn verify(&self, _: &RawToken) -> Result<AuthCtx, AuthError> {
        Ok(AuthCtx::anonymous())
    }
}

// ---------- task-local ----------

tokio::task_local! {
    /// The current request's [`AuthCtx`]. Set by the auth layer for the
    /// duration of each handler invocation. Generated client code reads
    /// this on outbound calls to propagate the caller's identity.
    ///
    /// **Spawn pitfall:** `CURRENT_AUTH` is task-local. If you
    /// `tokio::spawn` a future that calls a downstream service, capture
    /// `AuthCtx` first and pass it explicitly:
    ///
    /// ```ignore
    /// let auth = AuthCtx::from(&req);
    /// tokio::spawn(async move {
    ///     billing.do_thing_as(&auth, ...).await;
    /// });
    /// ```
    pub static CURRENT_AUTH: AuthCtx;
}

/// Convenience: read the current task's `AuthCtx`, returning anonymous
/// if no auth layer is active.
pub fn current() -> AuthCtx {
    CURRENT_AUTH
        .try_with(|a| a.clone())
        .unwrap_or_else(|_| AuthCtx::anonymous())
}

// ---------- service-token helper ----------

/// Mint a service-identity token using the configured
/// [`ServiceTokenMinter`]. For background jobs / queue consumers.
///
/// Defaults to [`default::HttpServiceTokenMinter::from_env`] if no
/// custom minter has been registered.
pub async fn service_token() -> Result<AuthCtx, AuthError> {
    static MINTER: tokio::sync::OnceCell<Arc<dyn ServiceTokenMinter>> =
        tokio::sync::OnceCell::const_new();
    let minter = MINTER
        .get_or_try_init(|| async {
            let m = default::HttpServiceTokenMinter::from_env()?;
            Ok::<Arc<dyn ServiceTokenMinter>, AuthError>(Arc::new(m))
        })
        .await?;
    minter.mint().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chain_verifier_first_success_wins() {
        struct AlwaysOk(AuthCtx);
        #[async_trait]
        impl TokenVerifier for AlwaysOk {
            async fn verify(&self, _: &RawToken) -> Result<AuthCtx, AuthError> {
                Ok(self.0.clone())
            }
        }
        struct AlwaysErr;
        #[async_trait]
        impl TokenVerifier for AlwaysErr {
            async fn verify(&self, _: &RawToken) -> Result<AuthCtx, AuthError> {
                Err(AuthError::Signature)
            }
        }
        let mut ok = AuthCtx::anonymous();
        ok.subject = "alice".into();

        let chain = ChainVerifier::new().add(AlwaysErr).add(AlwaysOk(ok));
        let token = RawToken {
            value: "x".into(),
            kind: "bearer-jwt",
        };
        let out = chain.verify(&token).await.unwrap();
        assert_eq!(out.subject, "alice");
    }

    #[tokio::test]
    async fn chain_verifier_returns_last_err_when_all_fail() {
        struct ErrA;
        struct ErrB;
        #[async_trait]
        impl TokenVerifier for ErrA {
            async fn verify(&self, _: &RawToken) -> Result<AuthCtx, AuthError> {
                Err(AuthError::Signature)
            }
        }
        #[async_trait]
        impl TokenVerifier for ErrB {
            async fn verify(&self, _: &RawToken) -> Result<AuthCtx, AuthError> {
                Err(AuthError::Expired)
            }
        }
        let chain = ChainVerifier::new().add(ErrA).add(ErrB);
        let token = RawToken {
            value: "x".into(),
            kind: "bearer-jwt",
        };
        let err = chain.verify(&token).await.unwrap_err();
        // Last err wins.
        matches!(err, AuthError::Expired);
    }
}
