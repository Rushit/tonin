//! tonin-sdk: Service builder, Config, Context, Error, and runtime for tonin microservices.
//!
//! # Usage
//!
//! ```toml
//! [dependencies]
//! tonin-sdk = "0.4"
//! ```
//!
//! ```no_run
//! use tonin_sdk::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> tonin_sdk::Result<()> {
//!     Service::new("greeter").run().await
//! }
//! ```
//!
//! # Example
//!
//! ```no_run
//! use tonin_sdk::{Service, Result};
//! use tonin_sdk::auth::default::JwtValidator;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let verifier = JwtValidator::from_env()?;
//!     Service::new("greeter")
//!         .with_auth(verifier)
//!         .enable_mcp()
//!         .handler(my_grpc_service())
//!         .run()
//!         .await
//! }
//!
//! # use tonic::body::BoxBody;
//! # #[derive(Clone)]
//! # struct MyGrpc;
//! # impl tonic::server::NamedService for MyGrpc {
//! #     const NAME: &'static str = "my.Grpc";
//! # }
//! # impl tower::Service<http::Request<BoxBody>> for MyGrpc {
//! #     type Response = http::Response<BoxBody>;
//! #     type Error = std::convert::Infallible;
//! #     type Future = std::pin::Pin<Box<dyn std::future::Future<
//! #         Output = std::result::Result<Self::Response, Self::Error>> + Send>>;
//! #     fn poll_ready(&mut self, _: &mut std::task::Context<'_>)
//! #         -> std::task::Poll<std::result::Result<(), Self::Error>> { unimplemented!() }
//! #     fn call(&mut self, _: http::Request<BoxBody>) -> Self::Future { unimplemented!() }
//! # }
//! # fn my_grpc_service() -> MyGrpc { MyGrpc }
//! ```
//!
//! # Modules
//!
//! - [`auth`] — token extraction, verification, [`auth::AuthCtx`]
//! - [`mcp`] — in-process MCP sidecar (second port, shared lifecycle)
//! - [`telemetry`] — zero-config OTLP tracing + W3C TraceContext
//! - [`transport`] — tonic/gRPC wiring helpers
//! - [`discovery`] — k8s DNS-based service resolution
//! - [`traits`] — capability traits (see below)
//! - [`state`] — pre-wired DB + cache connections from env at boot
//! - [`instrumented`] — OTel-span decorators around capability impls
//! - [`job`] — background job / queue consumer surface
//! - [`error`] — [`Error`] + [`Result`]
//!
//! # Capability traits
//!
//! [`traits::Cache`], [`traits::Database`], [`traits::EventBus`], and
//! [`traits::SecretStore`] are the interface-first surface every service
//! codes against. Concrete impls live in their own crates and are
//! selected at deploy time via `tonin.toml` `engine = "..."` — swapping
//! a backend is a TOML + `Cargo.toml` change, never a handler rewrite.
//!
//! # Sample app
//!
//! The canonical end-to-end example is
//! [`examples/greeter`](https://github.com/Rushit/tonin/tree/main/examples/greeter):
//! one proto, one `main.rs`, one `tonin.toml`.
//!
//! # Sibling crates
//!
//! - [`tonin`](https://docs.rs/tonin) — umbrella re-export + prelude
//! - [`tonin-client`](https://docs.rs/tonin-client) — peer-service client primitives
//! - [`tonin-mcp-macros`](https://docs.rs/tonin-mcp-macros) — `#[mcp_expose]` proc-macro
//! - [`tonin-build`](https://docs.rs/tonin-build) — `build.rs` helper around tonic-build

use std::net::SocketAddr;

use tower::layer::util::{Identity, Stack};

pub mod auth;
pub mod discovery;
pub mod error;
pub mod instrumented;
pub mod job;
pub mod mcp;
pub mod state;
pub mod telemetry;
pub mod traits;
pub mod transport;

pub use state::State;

pub use error::{Error, Result};

/// The tonic Router type after we install the trace-extract + auth
/// layers. Both run on every request: telemetry extracts trace context
/// first, then auth verifies the token.
type LayeredRouter = tonic::transport::server::Router<
    Stack<auth::AuthLayer, Stack<crate::telemetry::propagate::ExtractLayer, Identity>>,
>;

/// Type-erased factory that boots an MCP listener with a custom handler.
///
/// Service authors get one of these when they call
/// [`Service::enable_mcp_with`]; the framework calls the boxed closure
/// during `run()`. The handler type itself (e.g. `GreeterImplMcpAdapter`
/// emitted by `#[mcp_expose]`) is erased here so `Service` can stay
/// generic-free at the type level.
pub(crate) type McpSpawner = Box<
    dyn FnOnce(
            crate::mcp::McpConfig,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = std::result::Result<
                            (SocketAddr, tokio::task::JoinHandle<()>),
                            std::io::Error,
                        >,
                    > + Send,
            >,
        > + Send,
>;

/// Service builder. Holds name, bind address, the tonic router being
/// assembled, the optional auth layer, and the optional in-process MCP
/// listener config + custom spawner.
pub struct Service {
    name: String,
    addr: SocketAddr,
    router: Option<LayeredRouter>,
    auth_layer: Option<auth::AuthLayer>,
    mcp: Option<crate::mcp::McpConfig>,
    /// If set, called to spawn the MCP listener with a custom handler
    /// (e.g. `GreeterImplMcpAdapter` from `#[mcp_expose]`). If None,
    /// the default `McpServer` is used.
    mcp_spawner: Option<McpSpawner>,
}

impl Service {
    /// Construct a new service. Initializes OTLP tracing as a side effect
    /// (idempotent; disabled if `TONIN_TELEMETRY=off`).
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        // Zero-config telemetry. Errors are logged but do not block startup —
        // a service should still come up even if the collector is unreachable.
        if let Err(e) = crate::telemetry::init(&name) {
            eprintln!("micro: telemetry init failed: {e}");
        }
        Self {
            name,
            addr: "0.0.0.0:50051".parse().unwrap(),
            router: None,
            auth_layer: None,
            mcp: None,
            mcp_spawner: None,
        }
    }

    pub fn addr(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self
    }

    /// Enable the in-process MCP listener on the default port
    /// (`0.0.0.0:50052`). Same process, second port — MCP tool calls
    /// see the same `AuthCtx` task-local, share DB/cache/storage
    /// connections, and live or die with the gRPC server. To override
    /// the port use [`Service::mcp_addr`].
    ///
    /// What this currently does: spawns a hyper listener that answers
    /// `GET /health` with 200. The real MCP wire protocol (rmcp +
    /// `#[tool]` macros bridging each gRPC method) lands in a
    /// follow-up. This call only commits the framework to the
    /// "in-process, second port" lifecycle.
    pub fn enable_mcp(mut self) -> Self {
        self.mcp = Some(crate::mcp::McpConfig::default());
        self
    }

    /// Enable the in-process MCP listener on a specific address.
    /// Useful for tests (bind `:0` to grab a random free port) or
    /// for non-default deployments.
    pub fn mcp_addr(mut self, addr: SocketAddr) -> Self {
        self.mcp = Some(crate::mcp::McpConfig { addr });
        self
    }

    /// Enable the in-process MCP listener with a **custom handler**.
    /// Use this to wire a `#[tonin::mcp_expose]`-generated adapter
    /// into the framework so MCP `tools/list` exposes one tool per
    /// gRPC method on the user's impl.
    ///
    /// `factory` is invoked once per MCP session by rmcp's
    /// `StreamableHttpService` — returning a fresh handler instance
    /// each time (cheap if the handler is `Arc`-backed, which the
    /// macro-generated adapter is).
    ///
    /// Example:
    /// ```ignore
    /// let svc = Service::new("greeter")
    ///     .with_auth(auth::verifier())
    ///     .enable_mcp_with(move || {
    ///         Ok(GreeterImplMcpAdapter::new(GreeterImpl::new(state.clone())))
    ///     });
    /// ```
    pub fn enable_mcp_with<H, F>(mut self, factory: F) -> Self
    where
        H: crate::mcp::McpServerHandler + Clone + Send + Sync + 'static,
        F: Fn() -> std::result::Result<H, std::io::Error> + Send + Sync + Clone + 'static,
    {
        if self.mcp.is_none() {
            self.mcp = Some(crate::mcp::McpConfig::default());
        }
        let spawner: McpSpawner = Box::new(move |cfg| {
            Box::pin(async move { crate::mcp::spawn_with(cfg, factory).await })
        });
        self.mcp_spawner = Some(spawner);
        self
    }

    /// Install an auth verifier with the default
    /// [`auth::default::BearerHeaderExtractor`].
    ///
    /// Every inbound request runs the extractor → verifier chain. The
    /// resulting [`auth::AuthCtx`] is placed in request extensions AND
    /// in the [`auth::CURRENT_AUTH`] task-local for the handler's
    /// execution.
    ///
    /// For custom token extraction (cookies, custom headers, etc.) build
    /// an [`auth::AuthLayer`] directly and pass via
    /// [`Service::with_auth_layer`].
    pub fn with_auth<V: auth::TokenVerifier>(mut self, verifier: V) -> Self {
        let extractor = auth::default::BearerHeaderExtractor;
        self.auth_layer = Some(auth::AuthLayer::new(extractor, verifier));
        self
    }

    /// Install a fully-customized auth layer. Use this when you need a
    /// non-default token extractor (e.g. cookie-based auth).
    pub fn with_auth_layer(mut self, layer: auth::AuthLayer) -> Self {
        self.auth_layer = Some(layer);
        self
    }

    /// Run all requests as anonymous. Handlers can still read
    /// `AuthCtx::from(&req)` — they'll get
    /// `AuthCtx { kind: PrincipalKind::Anonymous, .. }`.
    ///
    /// For services that genuinely don't authenticate (read-only public
    /// APIs, internal-mesh-only with mTLS as the only check).
    pub fn without_auth(mut self) -> Self {
        let extractor = auth::default::BearerHeaderExtractor;
        let verifier = auth::AnonymousVerifier;
        self.auth_layer = Some(auth::AuthLayer::new(extractor, verifier).optional());
        self
    }

    /// Attach a tonic-generated service. Codegen will call this for the user.
    ///
    /// Server-side trace-context extraction is installed once, the first
    /// time a handler is added; every subsequent handler shares the layer.
    ///
    /// The auth layer (if configured via [`Service::with_auth`]) wraps
    /// each individual service via tonic's per-service layering at
    /// `run()` time, so it applies uniformly to every handler.
    pub fn handler<S>(mut self, svc: S) -> Self
    where
        S: tower::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<tonic::body::BoxBody>,
                Error = std::convert::Infallible,
            > + tonic::server::NamedService
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
    {
        let auth_layer = self
            .auth_layer
            .clone()
            .unwrap_or_else(default_anonymous_auth);
        let router: LayeredRouter = match self.router.take() {
            Some(r) => r.add_service(svc),
            None => tonic::transport::Server::builder()
                .layer(crate::telemetry::propagate::extract_layer())
                .layer(auth_layer)
                .add_service(svc),
        };
        self.router = Some(router);
        self
    }

    pub async fn run(self) -> Result<()> {
        let Service {
            name,
            addr,
            router,
            auth_layer: _,
            mcp,
            mcp_spawner,
        } = self;
        let router = router.ok_or_else(|| Error::Config("no handler registered".into()))?;
        tracing::info!(service = %name, %addr, "tonin service starting");

        // If MCP is enabled, spawn its listener in parallel. The
        // gRPC server is still the "main" path — if MCP fails to
        // bind we log loudly but don't abort, because a service that
        // serves gRPC without MCP is still useful. (Auth-critical
        // misconfig would have failed earlier at boot.)
        let _mcp_handle = if let Some(cfg) = mcp {
            // Use the custom spawner (from `enable_mcp_with`) if one
            // was supplied; otherwise default to the bare McpServer
            // with just the `health` tool.
            let spawn_result = if let Some(spawner) = mcp_spawner {
                spawner(cfg).await
            } else {
                crate::mcp::spawn(cfg).await
            };
            match spawn_result {
                Ok((bound, handle)) => {
                    tracing::info!(service = %name, mcp_addr = %bound, "mcp listener running");
                    Some(handle)
                }
                Err(e) => {
                    tracing::error!(service = %name, error = %e, "mcp listener failed to bind — continuing without MCP");
                    None
                }
            }
        } else {
            None
        };

        let serve_result = router.serve(addr).await;
        // Flush spans before the process exits.
        crate::telemetry::shutdown();
        serve_result?;
        Ok(())
    }
}

/// Default auth layer used when [`Service::with_auth`] / `without_auth`
/// were not called. Treats every request as anonymous; handlers still
/// see an `AuthCtx { kind: Anonymous }` via `AuthCtx::from(&req)`.
fn default_anonymous_auth() -> auth::AuthLayer {
    auth::AuthLayer::new(
        auth::default::BearerHeaderExtractor,
        auth::AnonymousVerifier,
    )
    .optional()
}

pub mod prelude {
    // NOTE: `Result` is intentionally NOT in this prelude. It would
    // shadow `std::result::Result` in user code, which breaks
    // macro-generated code (rmcp's tool macros, custom derives) that
    // emits unqualified `Result<T, E>` two-arg forms. Spell it as
    // `tonin_sdk::Result<T>` when you want the framework's alias.
    pub use super::{Error, Service, State};
    pub use crate::auth::{AuthCtx, AuthError, PrincipalKind};
    pub use crate::traits::{
        Cache, Database, EventBus, MessageId, SecretStore, StartPosition, SubscribeOptions,
    };
    pub use tonic::{Request, Response, Status};
}

/// Convenience re-export: `#[tonin_sdk::main]` as an alias for `#[tokio::main]`.
pub use tokio::main;
