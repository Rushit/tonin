//! The tower::Layer that runs auth on every inbound request.
//!
//! Pulls a `RawToken` via [`TokenExtractor`] → verifies it via
//! [`TokenVerifier`] → puts the resulting `AuthCtx` into request
//! extensions AND into the [`CURRENT_AUTH`] task-local for the
//! handler's execution.
//!
//! If the extractor returns [`AuthError::MissingToken`] and the layer
//! was configured with `optional = true` (via [`crate::Service::without_auth()`]),
//! it inserts an anonymous `AuthCtx` instead of rejecting. This is the
//! single seam that supports opt-out without forcing handlers to think
//! about it.

use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use http::{Request, Response};
use tonic::Status;
use tonic::body::BoxBody;
use tower::{Layer, Service};

use super::{AuthCtx, AuthError, CURRENT_AUTH, TokenExtractor, TokenVerifier};

/// Layer that installs auth on every incoming request.
#[derive(Clone)]
pub struct AuthLayer {
    extractor: Arc<dyn TokenExtractor>,
    verifier: Arc<dyn TokenVerifier>,
    /// If true, MissingToken → anonymous AuthCtx instead of 401.
    optional: bool,
}

impl AuthLayer {
    pub fn new<E, V>(extractor: E, verifier: V) -> Self
    where
        E: TokenExtractor,
        V: TokenVerifier,
    {
        Self {
            extractor: Arc::new(extractor),
            verifier: Arc::new(verifier),
            optional: false,
        }
    }

    /// Mark the layer as opt-out friendly: missing token → anonymous
    /// AuthCtx, no 401. Used by [`crate::Service::without_auth()`].
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            extractor: self.extractor.clone(),
            verifier: self.verifier.clone(),
            optional: self.optional,
        }
    }
}

#[derive(Clone)]
pub struct AuthService<S> {
    inner: S,
    extractor: Arc<dyn TokenExtractor>,
    verifier: Arc<dyn TokenVerifier>,
    optional: bool,
}

impl<S> Service<Request<BoxBody>> for AuthService<S>
where
    S: Service<Request<BoxBody>, Response = Response<BoxBody>> + Clone + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<BoxBody>;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<BoxBody>) -> Self::Future {
        let mut inner = self.inner.clone();
        let extractor = self.extractor.clone();
        let verifier = self.verifier.clone();
        let optional = self.optional;

        Box::pin(async move {
            // Build a MetadataMap view over the request headers. The
            // extractor only needs metadata; this keeps the trait
            // dyn-safe (no generic over body type).
            let metadata = metadata_from_headers(req.headers());

            let ctx = match extractor.extract(&metadata) {
                Ok(token) => match verifier.verify(&token).await {
                    Ok(ctx) => ctx,
                    Err(e) => return Ok(error_response(e)),
                },
                Err(AuthError::MissingToken) if optional => AuthCtx::anonymous(),
                Err(e) => return Ok(error_response(e)),
            };

            // Stuff into extensions for `AuthCtx::from(&req)` access.
            req.extensions_mut().insert(ctx.clone());

            // Run the handler with CURRENT_AUTH set so generated clients
            // pick up the caller's identity on outbound calls.
            CURRENT_AUTH.scope(ctx, inner.call(req)).await
        })
    }
}

/// Build a `MetadataMap` view from raw http headers. Tonic's
/// `MetadataMap::from_headers` does this — we just call it.
fn metadata_from_headers(h: &http::HeaderMap) -> tonic::metadata::MetadataMap {
    tonic::metadata::MetadataMap::from_headers(h.clone())
}

/// Encode an `AuthError` as a gRPC status response. tonic 0.12 exposes
/// `Status::into_http()` for this.
fn error_response(e: AuthError) -> Response<BoxBody> {
    let status: Status = e.into();
    status.into_http()
}
