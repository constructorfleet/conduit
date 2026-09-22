"""MCP server construction for Instrumenta.

Builds an `MCPServer` from the `mcp` SDK with the four built-in tools
registered. `app.py` mounts it twice on the FastAPI app — streamable-HTTP at
`/mcp/http` and SSE at `/mcp/sse` — behind per-transport toggles.

Aggregator wiring (upstream MCP clients contributing tools/prompts/resources)
lands in the next PR and registers additional tools on the same server
instance.
"""

from __future__ import annotations

from mcp.server.mcpserver import MCPServer

from . import builtins


#: The built-in tools, as (function, name, description).
#:
#: A table rather than four `add_tool` calls so that "which tools are
#: built-in?" has one answer. `/tools` needs it to tell a built-in from an
#: aggregated upstream tool, and a second hand-maintained list would drift the
#: first time a built-in is added -- which is how the UI came to hardcode
#: these four as permanently enabled.
_BUILTIN_TOOLS = (
    (
        builtins.http_fetch,
        "http.fetch",
        "Fetch an http/https URL (GET or HEAD) and return status, headers, and body.",
    ),
    (builtins.time_now, "time.now", "Return the current UTC wall-clock time."),
    (
        builtins.math_eval,
        "math.eval",
        "Evaluate a numeric expression restricted to digits and + - * / ( ) . e %.",
    ),
    (
        builtins.text_regex,
        "text.regex",
        "Find all regex matches of a pattern in text; returns full match plus groups.",
    ),
)

#: Names of the built-in tools, for callers that classify a tool by origin.
BUILTIN_TOOL_NAMES = frozenset(name for _fn, name, _desc in _BUILTIN_TOOLS)


def build_mcp_server() -> MCPServer:
    """Construct the MCP server and register built-in tools."""
    server = MCPServer(name="instrumenta", version="0.1.0")

    for function, name, description in _BUILTIN_TOOLS:
        server.add_tool(function, name=name, description=description)

    return server
