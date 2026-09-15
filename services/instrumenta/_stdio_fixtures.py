"""Runnable fake stdio MCP servers for `test_stdio.py`.

Spawned as a subprocess by the supervisor under test. Invoked as a plain
script (`<python> _stdio_fixtures.py [gate_file]`), so the child needs only
`mcp` and the stdlib on its interpreter — no `instrumenta` package on its
path.

A `gate_file` argument makes the first spawn fail and a later one succeed:
while the gate file is absent the child exits non-zero (a child that cannot
come up yet — a lost boot race); once a test creates it, the child runs a
real stdio MCP server exposing a single `echo` tool. That is what exercises
register-on-recovery without timing races.
"""

from __future__ import annotations

import os
import sys


def _main() -> None:
    gate = sys.argv[1] if len(sys.argv) > 1 else None
    if gate is not None and not os.path.exists(gate):
        sys.stderr.write("stdio fixture: gate absent, exiting\n")
        raise SystemExit(1)

    import anyio
    from mcp.server.mcpserver import MCPServer

    srv = MCPServer("fake-stdio")

    @srv.tool(name="echo", description="Echo the message back")
    async def echo(message: str) -> str:
        return f"echo: {message}"

    anyio.run(srv.run_stdio_async)


if __name__ == "__main__":
    _main()
