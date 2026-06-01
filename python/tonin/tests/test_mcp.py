"""Integration tests for the FastMCP-backed in-process MCP listener.

Boots the listener on a random localhost port, opens a real MCP client
over streamable HTTP, calls `tools/list` + `tools/call`, asserts the
default `health` tool is present and returns "ok".
"""

from __future__ import annotations

import asyncio
import socket

import pytest
from mcp.client.session import ClientSession
from mcp.client.streamable_http import streamablehttp_client

from tonin.mcp import McpConfig, build_default_server, spawn


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@pytest.mark.asyncio
async def test_default_server_has_health_tool() -> None:
    """The default FastMCP instance from build_default_server includes `health`."""
    cfg = McpConfig(host="127.0.0.1", port=8000)
    mcp = build_default_server(cfg)
    tools = await mcp.list_tools()
    names = {t.name for t in tools}
    assert "health" in names


@pytest.mark.asyncio
async def test_mcp_lists_health_and_calls_it_over_streamable_http() -> None:
    """Real wire path: streamable-http transport, MCP client, tools/list + tools/call."""
    port = _free_port()
    cfg = McpConfig(host="127.0.0.1", port=port)
    _, task = await spawn(cfg)
    try:
        # Give uvicorn a moment to bind.
        await asyncio.sleep(0.5)

        url = f"http://127.0.0.1:{port}/mcp"
        async with streamablehttp_client(url) as (read, write, _get_session_id):
            async with ClientSession(read, write) as session:
                await session.initialize()
                tools = await session.list_tools()
                names = {t.name for t in tools.tools}
                assert "health" in names

                result = await session.call_tool("health")
                # CallToolResult.content is a list of text/image blocks.
                assert any(
                    getattr(block, "text", None) == "ok" for block in result.content
                )
    finally:
        task.cancel()
        try:
            await task
        except (asyncio.CancelledError, Exception):
            pass
