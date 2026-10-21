"""In-process MCP listener — Python side, real wire protocol via MCPServer.

Mirror of `tonin-mcp` (Rust). Spawned by :meth:`tonin.Service.enable_mcp`
on a configurable second port (default :50052). Lives in the same
asyncio event loop as the gRPC server, so:

  - The same ``contextvars.CURRENT_AUTH`` is visible to MCP tool calls
  - asyncpg / redis pools are shared
  - One container, two ports — k8s deployment stays simple

Transport: **streamable HTTP** (the MCP spec's recommended production
transport). MCPServer (the mcp 2.x successor to FastMCP) wraps a Starlette app served by uvicorn; we run
``mcp.run_streamable_http_async()`` as a tokio-style background task on
the asyncio loop.

The default :class:`McpServer` exposes one tool — ``health`` — that
returns ``"ok"``. Service authors register additional tools by
extending the returned instance::

    svc.enable_mcp()
    # ... or via direct access:
    mcp = svc.mcp_server()
    @mcp.tool()
    async def my_tool(name: str) -> str:
        return f"hello {name}"

The auto-derivation of one MCP tool per gRPC method lives in a
follow-up — for now the framework only ships the architectural
primitive + ``health``.
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass

from mcp.server.mcpserver import MCPServer

logger = logging.getLogger(__name__)


@dataclass(slots=True)
class McpConfig:
    """Configuration for the in-process MCP listener.

    Default address ``0.0.0.0:50052``. The streamable-http endpoint is
    served at ``/mcp`` (MCPServer's default).
    """

    host: str = "0.0.0.0"
    port: int = 50052


def build_default_server(config: McpConfig) -> MCPServer:
    """Construct an MCPServer instance pre-loaded with the ``health`` tool.

    Returns the instance unwrapped so framework users can attach more
    tools before :func:`spawn` actually starts the listener. The bind
    address in ``config`` is applied by :func:`spawn` (mcp 2.x takes
    transport settings at run time, not construction time).
    """
    del config  # bind address/transport settings are applied in spawn()
    mcp = MCPServer(
        "micro",
        instructions="micro service MCP endpoint. Tools available via tools/list.",
    )

    @mcp.tool(description="Liveness probe. Returns 'ok' if the service is running.")
    async def health() -> str:
        return "ok"

    return mcp


async def spawn(
    cfg: McpConfig, mcp: MCPServer | None = None
) -> tuple[tuple[str, int], asyncio.Task]:
    """Start the MCP listener as a background task on the running loop.

    Returns ``((host, port), task)`` — the bound address and a task
    that runs the streamable-http server. Cancel the task to stop.

    If ``mcp`` is None, a default server with the ``health`` tool is
    built. Otherwise the caller's pre-configured MCPServer is used —
    handy for tests and for framework users who want to register
    additional tools before the listener starts.
    """
    if mcp is None:
        mcp = build_default_server(cfg)

    logger.info("mcp listener starting host=%s port=%s", cfg.host, cfg.port)

    # MCPServer.run_streamable_http_async builds a uvicorn server and
    # blocks until shutdown. We spawn it as a task so the gRPC server
    # can run in parallel on the same loop.
    task = asyncio.create_task(
        mcp.run_streamable_http_async(
            host=cfg.host,
            port=cfg.port,
            stateless_http=True,  # recommended for production
            json_response=True,  # less framing overhead than SSE
        )
    )

    # No clean way to get the actual bound port out of MCPServer today
    # (uvicorn doesn't expose it via MCPServer's API surface). Return
    # the configured values — for tests use a fixed port + an
    # explicit health check on it.
    return (cfg.host, cfg.port), task
