//! Default implementations of the auth traits. Users import what they
//! want; swapping them out means dropping the corresponding deps from
//! `Cargo.toml`.
//!
//! - [`BearerHeaderExtractor`] — reads `Authorization: Bearer <token>`
//! - [`JwtValidator`] — full JWT validation (sig + exp + iss + aud) with
//!   JWKS caching
//! - [`HttpServiceTokenMinter`] — POSTs to an auth-service endpoint to
//!   obtain a service-identity token

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tonic::metadata::MetadataMap;

use super::{
    AuthCtx, AuthError, PrincipalKind, RawToken, ServiceTokenMinter, TokenExtractor, TokenVerifier,
};

// ---------- BearerHeaderExtractor ----------

/// Reads `Authorization: Bearer <token>` from request metadata.
#[derive(Clone, Copy, Debug, Default)]
pub struct BearerHeaderExtractor;

impl TokenExtractor for BearerHeaderExtractor {
    fn extract(&self, metadata: &MetadataMap) -> Result<RawToken, AuthError> {
        let header = metadata
            .get("authorization")
            .ok_or(AuthError::MissingToken)?;
        let value = header.to_str().map_err(|_| AuthError::MissingToken)?;
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .ok_or(AuthError::MissingToken)?
            .trim();
        if token.is_empty() {
            return Err(AuthError::MissingToken);
        }
        Ok(RawToken {
            value: token.to_string(),
            kind: "bearer-jwt",
        })
    }
}

// ---------- JwtValidator ----------

/// JWT validator with JWKS caching.
///
/// Configuration via env (consumed by [`Self::from_env`]):
/// - `TONIN_AUTH_ISSUER` — required `iss` claim
/// - `TONIN_AUTH_AUDIENCE` — required `aud` claim
/// - `TONIN_AUTH_JWKS_URL` — public-key endpoint; refreshed on cache miss
///   (and at most once per `TONIN_AUTH_JWKS_TTL_SECS`, default 600)
/// - `TONIN_AUTH_INSECURE_DEV` — when `=1`, skip signature/expiry/aud/iss
///   and accept any well-formed JWT. **Logs a loud warning at startup
///   and on every call.** Local dev only.
pub struct JwtValidator {
    config: JwtConfig,
    keys: Arc<RwLock<JwksCache>>,
    http: reqwest::Client,
}

#[derive(Clone)]
struct JwtConfig {
    issuer: String,
    audience: String,
    jwks_url: Option<String>,
    jwks_ttl: Duration,
    insecure_dev: bool,
    /// Static decoding key for unit tests: when set, skip JWKS fetch and
    /// use this key directly. Never set via env; only by test helpers.
    /// (`DecodingKey` itself is not `Debug`, hence no `Debug` derive
    /// on the struct.)
    static_key: Option<DecodingKey>,
    /// Algorithm for static-key tests. JWKS-served keys carry their own alg.
    static_alg: Algorithm,
}

#[derive(Default)]
struct JwksCache {
    /// key-id → DecodingKey
    keys: HashMap<String, DecodingKey>,
    fetched_at: Option<SystemTime>,
}

impl JwtValidator {
    /// Build from environment variables. See type docs for the recognized vars.
    pub fn from_env() -> Result<Self, AuthError> {
        let insecure_dev = std::env::var("TONIN_AUTH_INSECURE_DEV").ok().as_deref() == Some("1");
        let issuer = std::env::var("TONIN_AUTH_ISSUER").ok();
        let audience = std::env::var("TONIN_AUTH_AUDIENCE").ok();
        let jwks_url = std::env::var("TONIN_AUTH_JWKS_URL").ok();
        let ttl_secs = std::env::var("TONIN_AUTH_JWKS_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(600);

        if insecure_dev {
            tracing::warn!(
                "TONIN_AUTH_INSECURE_DEV=1 — JWT signatures NOT verified. Local dev only."
            );
            return Ok(Self::insecure_dev_inner(
                issuer.unwrap_or_default(),
                audience.unwrap_or_default(),
            ));
        }

        let issuer = issuer.ok_or_else(|| {
            AuthError::Config(
                "TONIN_AUTH_ISSUER unset (set TONIN_AUTH_INSECURE_DEV=1 for dev)".into(),
            )
        })?;
        let audience =
            audience.ok_or_else(|| AuthError::Config("TONIN_AUTH_AUDIENCE unset".into()))?;
        let jwks_url =
            jwks_url.ok_or_else(|| AuthError::Config("TONIN_AUTH_JWKS_URL unset".into()))?;

        Ok(Self {
            config: JwtConfig {
                issuer,
                audience,
                jwks_url: Some(jwks_url),
                jwks_ttl: Duration::from_secs(ttl_secs),
                insecure_dev: false,
                static_key: None,
                static_alg: Algorithm::RS256,
            },
            keys: Arc::new(RwLock::new(JwksCache::default())),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|e| AuthError::Config(format!("http client init: {e}")))?,
        })
    }

    /// Parse-only fallback: well-formed JWT accepted, no signature check.
    /// **Local dev only.** Triggered by `TONIN_AUTH_INSECURE_DEV=1` or by
    /// calling this constructor directly.
    pub fn insecure_dev() -> Self {
        Self::insecure_dev_inner(String::new(), String::new())
    }

    fn insecure_dev_inner(issuer: String, audience: String) -> Self {
        Self {
            config: JwtConfig {
                issuer,
                audience,
                jwks_url: None,
                jwks_ttl: Duration::from_secs(0),
                insecure_dev: true,
                static_key: None,
                static_alg: Algorithm::RS256,
            },
            keys: Arc::new(RwLock::new(JwksCache::default())),
            http: reqwest::Client::new(),
        }
    }

    /// Test helper: build a validator that uses a fixed `DecodingKey` and
    /// skips JWKS fetch. Audience + issuer still enforced.
    #[cfg(test)]
    pub(crate) fn with_static_key(
        issuer: String,
        audience: String,
        key: DecodingKey,
        alg: Algorithm,
    ) -> Self {
        Self {
            config: JwtConfig {
                issuer,
                audience,
                jwks_url: None,
                jwks_ttl: Duration::from_secs(0),
                insecure_dev: false,
                static_key: Some(key),
                static_alg: alg,
            },
            keys: Arc::new(RwLock::new(JwksCache::default())),
            http: reqwest::Client::new(),
        }
    }

    async fn resolve_key(&self, kid: Option<&str>) -> Result<DecodingKey, AuthError> {
        // Static key takes precedence (tests).
        if let Some(k) = &self.config.static_key {
            return Ok(k.clone());
        }
        let jwks_url = self
            .config
            .jwks_url
            .as_deref()
            .ok_or_else(|| AuthError::Config("no JWKS URL configured".into()))?;

        // Cache hit?
        if let Some(kid) = kid {
            let cache = self.keys.read().expect("jwks cache poisoned");
            if let Some(k) = cache.keys.get(kid)
                && let Some(fetched) = cache.fetched_at
                && SystemTime::now()
                    .duration_since(fetched)
                    .unwrap_or_default()
                    < self.config.jwks_ttl
            {
                return Ok(k.clone());
            }
        }

        // Miss → refresh.
        self.refresh_jwks(jwks_url).await?;

        let cache = self.keys.read().expect("jwks cache poisoned");
        match kid {
            Some(k) => cache
                .keys
                .get(k)
                .cloned()
                .ok_or_else(|| AuthError::Verification(format!("no JWKS key for kid={k}"))),
            None => cache
                .keys
                .values()
                .next()
                .cloned()
                .ok_or_else(|| AuthError::Verification("JWKS empty".into())),
        }
    }

    async fn refresh_jwks(&self, url: &str) -> Result<(), AuthError> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(AuthError::Transport(format!(
                "JWKS fetch failed: HTTP {}",
                resp.status()
            )));
        }
        let jwks: Jwks = resp
            .json()
            .await
            .map_err(|e| AuthError::Verification(format!("JWKS parse: {e}")))?;

        let mut new_keys = HashMap::new();
        for k in jwks.keys {
            if let (Some(kid), Some(n), Some(e)) = (k.kid, k.n, k.e)
                && let Ok(dk) = DecodingKey::from_rsa_components(&n, &e)
            {
                new_keys.insert(kid, dk);
            }
        }

        let mut cache = self.keys.write().expect("jwks cache poisoned");
        cache.keys = new_keys;
        cache.fetched_at = Some(SystemTime::now());
        Ok(())
    }
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

/// Standard JWT claims we extract into `AuthCtx`.
#[derive(Deserialize, Debug)]
struct Claims {
    sub: String,
    iss: String,
    aud: AudClaim,
    exp: i64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    /// `kind` is non-standard; a custom claim distinguishing user tokens from service tokens.
    #[serde(default)]
    kind: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

/// `aud` may be a string or array. Handle both.
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum AudClaim {
    Single(String),
    Multi(Vec<String>),
}

impl AudClaim {
    fn first(&self) -> String {
        match self {
            AudClaim::Single(s) => s.clone(),
            AudClaim::Multi(v) => v.first().cloned().unwrap_or_default(),
        }
    }
}

#[async_trait]
impl TokenVerifier for JwtValidator {
    async fn verify(&self, token: &RawToken) -> Result<AuthCtx, AuthError> {
        if self.config.insecure_dev {
            return verify_insecure(&token.value, &self.config);
        }

        let header = decode_header(&token.value).map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::InvalidToken => {
                AuthError::Verification("malformed".into())
            }
            _ => AuthError::Verification(e.to_string()),
        })?;

        let key = self.resolve_key(header.kid.as_deref()).await?;
        let alg = if self.config.static_key.is_some() {
            self.config.static_alg
        } else {
            header.alg
        };

        let mut validation = Validation::new(alg);
        validation.set_audience(&[&self.config.audience]);
        validation.set_issuer(&[&self.config.issuer]);
        validation.validate_exp = true;

        let data =
            decode::<Claims>(&token.value, &key, &validation).map_err(|e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::InvalidSignature => AuthError::Signature,
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::Expired,
                jsonwebtoken::errors::ErrorKind::InvalidAudience => AuthError::Audience {
                    expected: self.config.audience.clone(),
                    got: "(rejected by validator)".into(),
                },
                jsonwebtoken::errors::ErrorKind::InvalidIssuer => AuthError::Issuer {
                    expected: self.config.issuer.clone(),
                    got: "(rejected by validator)".into(),
                },
                _ => AuthError::Verification(e.to_string()),
            })?;

        Ok(claims_to_authctx(data.claims, &token.value))
    }
}

fn verify_insecure(jwt: &str, cfg: &JwtConfig) -> Result<AuthCtx, AuthError> {
    // Parse-only: split, base64-decode the payload, deserialize.
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err(AuthError::Verification("not a JWT".into()));
    }
    let payload = base64_url_decode(parts[1])
        .map_err(|e| AuthError::Verification(format!("payload base64: {e}")))?;
    let claims: Claims = serde_json::from_slice(&payload)
        .map_err(|e| AuthError::Verification(format!("payload json: {e}")))?;
    let ctx = claims_to_authctx(claims, jwt);
    // Best-effort issuer/audience enforcement even in dev, if configured.
    if !cfg.issuer.is_empty() && ctx.issuer != cfg.issuer {
        return Err(AuthError::Issuer {
            expected: cfg.issuer.clone(),
            got: ctx.issuer,
        });
    }
    if !cfg.audience.is_empty() && ctx.audience != cfg.audience {
        return Err(AuthError::Audience {
            expected: cfg.audience.clone(),
            got: ctx.audience,
        });
    }
    tracing::warn!(subject = %ctx.subject, "INSECURE_DEV: accepted unsigned JWT");
    Ok(ctx)
}

fn base64_url_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| e.to_string())
}

fn claims_to_authctx(c: Claims, raw: &str) -> AuthCtx {
    let mut scopes = Vec::new();
    if let Some(s) = c.scope {
        scopes.extend(s.split_whitespace().map(String::from));
    }
    if let Some(v) = c.scopes {
        scopes.extend(v);
    }
    let kind = match c.kind.as_deref() {
        Some("service") => PrincipalKind::Service,
        Some("agent") => PrincipalKind::Agent,
        _ => PrincipalKind::User,
    };
    AuthCtx {
        subject: c.sub,
        issuer: c.iss,
        audience: c.aud.first(),
        scopes,
        kind,
        raw_token: raw.to_string(),
        expires_at: c.exp.max(0) as f64,
        extra: c.extra,
    }
}

// ---------- HttpServiceTokenMinter ----------

/// Mints service-identity tokens by POSTing to an auth-service endpoint.
///
/// Configuration:
/// - `TONIN_AUTH_SERVICE_TOKEN_URL` — endpoint to POST to (required)
/// - `TONIN_AUTH_SERVICE_AUDIENCE` — `aud` to request (defaults to service's name)
/// - `TONIN_AUTH_SERVICE_TOKEN_SCOPES` — comma-separated scopes
///
/// The minter caches the token in memory and refreshes 60s before expiry.
pub struct HttpServiceTokenMinter {
    url: String,
    audience: String,
    scopes: Vec<String>,
    http: reqwest::Client,
    cached: tokio::sync::RwLock<Option<AuthCtx>>,
}

impl HttpServiceTokenMinter {
    pub fn from_env() -> Result<Self, AuthError> {
        let url = std::env::var("TONIN_AUTH_SERVICE_TOKEN_URL")
            .map_err(|_| AuthError::Config("TONIN_AUTH_SERVICE_TOKEN_URL unset".into()))?;
        let audience = std::env::var("TONIN_AUTH_SERVICE_AUDIENCE").unwrap_or_default();
        let scopes = std::env::var("TONIN_AUTH_SERVICE_TOKEN_SCOPES")
            .ok()
            .map(|s| s.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        Ok(Self {
            url,
            audience,
            scopes,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|e| AuthError::Config(format!("http client: {e}")))?,
            cached: tokio::sync::RwLock::new(None),
        })
    }
}

#[derive(serde::Serialize)]
struct MintRequest<'a> {
    audience: &'a str,
    scopes: &'a [String],
}

#[derive(Deserialize)]
struct MintResponse {
    token: String,
    /// Optional expiry hint in seconds; not strictly required.
    #[serde(default)]
    expires_in: Option<u64>,
}

#[async_trait]
impl ServiceTokenMinter for HttpServiceTokenMinter {
    async fn mint(&self) -> Result<AuthCtx, AuthError> {
        // Return cached if still valid (>60s remaining).
        {
            let cached = self.cached.read().await;
            if let Some(ctx) = cached.as_ref() {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                if ctx.expires_at - now > 60.0 {
                    return Ok(ctx.clone());
                }
            }
        }

        let body = MintRequest {
            audience: &self.audience,
            scopes: &self.scopes,
        };
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(AuthError::Transport(format!(
                "service-token mint failed: HTTP {}",
                resp.status()
            )));
        }
        let body: MintResponse = resp
            .json()
            .await
            .map_err(|e| AuthError::Verification(format!("mint response: {e}")))?;

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let expires_at = now_secs + body.expires_in.unwrap_or(3600) as f64;

        let ctx = AuthCtx {
            subject: "service".into(),
            issuer: "micro-auth-svc".into(),
            audience: self.audience.clone(),
            scopes: self.scopes.clone(),
            kind: PrincipalKind::Service,
            raw_token: body.token,
            expires_at,
            extra: HashMap::new(),
        };
        *self.cached.write().await = Some(ctx.clone());
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};

    fn signing_keypair() -> (EncodingKey, DecodingKey) {
        // Reuse the test key from jsonwebtoken's own examples — generated
        // freshly here to avoid pulling in a private-key file.
        // RSA-2048 keys: encoding from PEM, decoding from PEM.
        // For test simplicity, use HS256 (symmetric) — same code path,
        // just a different algorithm. JWKS path is exercised separately
        // via integration tests.
        let secret = b"a-test-secret-at-least-32-bytes-long-please";
        (
            EncodingKey::from_secret(secret),
            DecodingKey::from_secret(secret),
        )
    }

    fn build_jwt(
        signing: &EncodingKey,
        sub: &str,
        iss: &str,
        aud: &str,
        scopes: &[&str],
        ttl_secs: i64,
    ) -> String {
        #[derive(serde::Serialize)]
        struct Cl<'a> {
            sub: &'a str,
            iss: &'a str,
            aud: &'a str,
            exp: i64,
            scope: String,
        }
        let exp = chrono_now() + ttl_secs;
        let cl = Cl {
            sub,
            iss,
            aud,
            exp,
            scope: scopes.join(" "),
        };
        encode(&Header::new(Algorithm::HS256), &cl, signing).unwrap()
    }

    fn chrono_now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[tokio::test]
    async fn jwt_validator_accepts_valid_token() {
        let (signing, verifying) = signing_keypair();
        let v = JwtValidator::with_static_key(
            "https://auth.example.com".into(),
            "billing-service".into(),
            verifying,
            Algorithm::HS256,
        );
        let jwt = build_jwt(
            &signing,
            "alice",
            "https://auth.example.com",
            "billing-service",
            &["read:billing", "write:billing"],
            300,
        );
        let token = RawToken {
            value: jwt,
            kind: "bearer-jwt",
        };
        let ctx = v.verify(&token).await.unwrap();
        assert_eq!(ctx.subject, "alice");
        assert_eq!(ctx.audience, "billing-service");
        assert!(ctx.scopes.contains(&"read:billing".to_string()));
    }

    #[tokio::test]
    async fn jwt_validator_rejects_expired_token() {
        let (signing, verifying) = signing_keypair();
        let v = JwtValidator::with_static_key(
            "https://auth.example.com".into(),
            "billing-service".into(),
            verifying,
            Algorithm::HS256,
        );
        // jsonwebtoken's Validation has a default leeway of 60s. Use a
        // much-larger negative ttl so we land safely outside it.
        let jwt = build_jwt(
            &signing,
            "alice",
            "https://auth.example.com",
            "billing-service",
            &[],
            -3600,
        );
        let token = RawToken {
            value: jwt,
            kind: "bearer-jwt",
        };
        let err = v.verify(&token).await.unwrap_err();
        assert!(matches!(err, AuthError::Expired), "got {err:?}");
    }

    #[tokio::test]
    async fn jwt_validator_rejects_wrong_audience() {
        let (signing, verifying) = signing_keypair();
        let v = JwtValidator::with_static_key(
            "https://auth.example.com".into(),
            "billing-service".into(),
            verifying,
            Algorithm::HS256,
        );
        let jwt = build_jwt(
            &signing,
            "alice",
            "https://auth.example.com",
            "WRONG",
            &[],
            300,
        );
        let token = RawToken {
            value: jwt,
            kind: "bearer-jwt",
        };
        let err = v.verify(&token).await.unwrap_err();
        assert!(matches!(err, AuthError::Audience { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn jwt_validator_rejects_bad_signature() {
        let (_signing, verifying) = signing_keypair();
        let (other_signing, _) = signing_keypair();
        // Tamper: change the verifying key to something unrelated.
        let v = JwtValidator::with_static_key(
            "https://auth.example.com".into(),
            "billing-service".into(),
            DecodingKey::from_secret(b"different-secret-also-32-bytes-or-more!"),
            Algorithm::HS256,
        );
        let jwt = build_jwt(
            &other_signing,
            "alice",
            "https://auth.example.com",
            "billing-service",
            &[],
            300,
        );
        let token = RawToken {
            value: jwt,
            kind: "bearer-jwt",
        };
        let err = v.verify(&token).await.unwrap_err();
        assert!(matches!(err, AuthError::Signature), "got {err:?}");
        let _ = verifying; // unused
    }

    #[test]
    fn bearer_extractor_parses_authorization() {
        let mut md = MetadataMap::new();
        md.insert("authorization", "Bearer test-token".parse().unwrap());
        let t = BearerHeaderExtractor.extract(&md).unwrap();
        assert_eq!(t.value, "test-token");
        assert_eq!(t.kind, "bearer-jwt");
    }

    #[test]
    fn bearer_extractor_missing_header() {
        let md = MetadataMap::new();
        let err = BearerHeaderExtractor.extract(&md).unwrap_err();
        assert!(matches!(err, AuthError::MissingToken));
    }
}
