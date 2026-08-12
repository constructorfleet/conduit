"""MCP server construction for Instrumenta.

Builds an `MCPServer` from the `mcp` SDK with the four built-in tools
registered. The resulting Starlette sub-app is mounted on the FastAPI app at
`/mcp` by `app.py`.

Aggregator wiring (upstream MCP clients contributing tools/prompts/resources)
lands in the next PR and registers additional tools on the same server
instance.
"""

from __future__ import annotations

from mcp.server.mcpserver import MCPServer

from . import builtins


def build_mcp_server() -> MCPServer:
    """Construct the MCP server and register built-in tools."""
    server = MCPServer(name="instrumenta", version="0.1.0")

    server.add_tool(
        builtins.http_fetch,
        name="http.fetch",
        description="Fetch an http/https URL (GET or HEAD) and return status, headers, and body.",
    )
    server.add_tool(
        builtins.time_now,
        name="time.now",
        description="Return the current UTC wall-clock time.",
    )
    server.add_tool(
        builtins.math_eval,
        name="math.eval",
        description="Evaluate a numeric expression restricted to digits and + - * / ( ) . e %.",
    )
    server.add_tool(
        builtins.text_regex,
        name="text.regex",
        description="Find all regex matches of a pattern in text; returns full match plus groups.",
    )

    return server
