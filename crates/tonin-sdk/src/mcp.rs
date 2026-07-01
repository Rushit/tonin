//! In-process MCP listener — real wire protocol via `rmcp`.
//!
//! Spawned by [`spawn`] when the framework's `Service` builder
//! has `.enable_mcp()`. Same process as the gRPC server, second port
//! (default `:50052`).
//!
//! ## What's implemented now
//!
//! - rmcp 1.x [`StreamableHttpService`] hosted over hyper
//! - A default `McpServer` with one `health` tool — verifiable
//!   end-to-end via the official MCP wire protocol
//! - Extension surface ([`McpServer`] is `pub`) so framework users can
//!   add tools by deriving on it or wrapping it
//!
//! ## What's next
//!
//! Auto-derivation of one MCP tool per gRPC method using proto
//! descriptors. Tracked in the design doc; not in this commit.
//!
//! ## Why streamable-HTTP and not stdio
//!
//! Stdio is per-process — a fresh client subprocess per MCP session.
//! That breaks the "second port on the same long-running service"
//! model we're after. Streamable-HTTP lives on a TCP port; multiple
//! MCP clients can connect concurrently to the same running service.

use std::net::SocketAddr;
use std::sync::Arc;

use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HyperBuilder;
use hyper_util::service::TowerToHyperService;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use tokio::net::TcpListener;

// Re-export the proc-macro so user crates see `#[tonin::mcp_expose]`
// without an extra dep.
pub use tonin_mcp_macros::mcp_expose;

// Re-export rmcp surface that adapter structs reference. The
// proc-macro `#[mcp_expose]` emits paths like
// `::tonin::mcp::__rmcp_reexport::tool` so user crates only need
// `tonin` as a direct dep — they don't have to pin rmcp themselves.
#[doc(hidden)]
pub use rmcp as __rmcp_reexport;

pub use rmcp::handler::server::wrapper::Parameters;
pub use rmcp::model::CallToolResult as McpCallToolResult;
pub use rmcp::model::ContentBlock as McpContent;
pub use rmcp::{ErrorData as McpErrorData, ServerHandler as McpServerHandler};

/// Configuration for the in-process MCP listener.
#[derive(Clone, Debug)]
pub struct McpConfig {
    pub addr: SocketAddr,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:50052".parse().expect("valid default mcp addr"),
        }
    }
}

/// Default MCP server. Carries the `tool_router` field that rmcp's
/// `#[tool_router]` macro populates with method handlers.
///
/// Service authors can:
///   - Use this as-is (just the `health` tool).
///   - Wrap it in their own struct and forward the trait impl.
///   - Replace it entirely by hand-rolling a `ServerHandler`.
///
/// Cloning is cheap — the router itself is reference-counted.
#[derive(Clone)]
pub struct McpServer {
    // Required by the `#[tool_handler]` macro — read indirectly when
    // the macro expands into `ServerHandler::call_tool` / `list_tools`.
    #[allow(dead_code)]
    tool_router: ToolRouter<McpServer>,
}

#[tool_router]
impl McpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Cheap liveness probe. Returns `"ok"` if the service is running.
    /// Mirrors what `GET /health` did in the stub — useful for the
    /// same orchestration probes during the rollout.
    #[tool(description = "Liveness probe. Returns 'ok' if the service is running.")]
    async fn health(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text("ok")]))
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "tonin service MCP endpoint. Tools available via tools/list.".to_string(),
            )
    }
}

/// Bind a TCP listener and serve rmcp's streamable-HTTP transport on it
/// using the default [`McpServer`] (only the `health` tool).
///
/// Most callers want [`spawn_with`] so they can supply an adapter
/// containing macro-generated tools.
pub async fn spawn(
    cfg: McpConfig,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), std::io::Error> {
    spawn_with(cfg, || Ok::<McpServer, std::io::Error>(McpServer::new())).await
}

/// Bind + serve with a user-supplied handler factory. Each MCP session
/// invokes `factory` to mint a fresh handler instance, matching rmcp's
/// per-session lifecycle expectation.
///
/// Used by `Service::handler` to spawn an MCP server backed by the
/// `<Impl>McpAdapter` generated by `#[mcp_expose]`.
pub async fn spawn_with<H, F>(
    cfg: McpConfig,
    factory: F,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), std::io::Error>
where
    H: ServerHandler + Clone + Send + Sync + 'static,
    F: Fn() -> Result<H, std::io::Error> + Send + Sync + 'static + Clone,
{
    let listener = TcpListener::bind(cfg.addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!(target: "tonin::mcp", addr = %bound, "mcp listener bound");

    let session_mgr = Arc::new(LocalSessionManager::default());
    let streamable = StreamableHttpService::new(factory, session_mgr, Default::default());
    let tower_svc = TowerToHyperService::new(streamable);

    let handle = tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(target: "tonin::mcp", error = %e, "accept failed");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            let svc = tower_svc.clone();
            tokio::spawn(async move {
                if let Err(e) = HyperBuilder::new(TokioExecutor::new())
                    .serve_connection(io, svc)
                    .await
                {
                    tracing::debug!(target: "tonin::mcp", %peer, error = %e, "connection ended");
                }
            });
        }
    });

    Ok((bound, handle))
}
