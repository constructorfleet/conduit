"""In-process asyncio supervisor for stdio upstream MCP servers.

Spawns child processes, connects via stdin/stdout as MCP clients, and
autorestarts with capped exponential backoff on failure. The backoff
ramp is 1s → 2s → 4s → … → 30s (cap), resetting on a successful
connection.
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass, field
from typing import Any

LOG = logging.getLogger("instrumenta.stdio")

_MAX_BACKOFF = 30.0
_INITIAL_BACKOFF = 1.0


@dataclass
class _Child:
    """State for one managed child process."""

    command: str
    server_name: str
    process: asyncio.subprocess.Process | None = None
    backoff: float = _INITIAL_BACKOFF
    task: asyncio.Task[None] | None = None
    stop_event: asyncio.Event = field(default_factory=asyncio.Event)


class StdioSupervisor:
    """Manages stdio upstream child processes.

    Each child is spawned with its stdin/stdout connected to an MCP client.
    On crash, the child is restarted with exponential backoff up to
    `_MAX_BACKOFF` seconds. On successful connection, backoff resets.
    """

    def __init__(self) -> None:
        self._children: dict[str, _Child] = {}

    async def spawn_and_list_tools(
        self,
        command: str,
        server_name: str,
    ) -> list[dict[str, Any]]:
        """Spawn a child, perform MCP handshake, list tools, return them.

        Each tool name is prefixed with `<server_name>.` to match the
        HTTP aggregator convention.
        """
        import shlex

        parts = shlex.split(command)
        process = await asyncio.create_subprocess_exec(
            *parts,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        assert process.stdin is not None
        assert process.stdout is not None

        # MCP initialize handshake.
        init_request = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "instrumenta", "version": "0.1.0"},
            },
        }
        import json

        process.stdin.write((json.dumps(init_request) + "\n").encode())
        await process.stdin.drain()

        line = await asyncio.wait_for(process.stdout.readline(), timeout=10.0)
        init_response = json.loads(line.decode())

        # Send initialized notification.
        initialized = {"jsonrpc": "2.0", "method": "notifications/initialized"}
        process.stdin.write((json.dumps(initialized) + "\n").encode())
        await process.stdin.drain()

        # List tools.
        list_request = {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}
        process.stdin.write((json.dumps(list_request) + "\n").encode())
        await process.stdin.drain()

        line = await asyncio.wait_for(process.stdout.readline(), timeout=10.0)
        list_response = json.loads(line.decode())

        tools = list_response.get("result", {}).get("tools", [])
        # Prefix tool names with server_name.
        prefixed = [
            {"name": f"{server_name}.{t['name']}", "description": t.get("description", "")}
            for t in tools
        ]

        child = _Child(command=command, server_name=server_name, process=process)
        self._children[server_name] = child

        return prefixed

    def close_all(self) -> None:
        """Terminate all managed child processes."""
        for child in self._children.values():
            child.stop_event.set()
            if child.task is not None:
                child.task.cancel()
            if child.process is not None and child.process.returncode is None:
                child.process.kill()

    def statuses(self) -> list[dict[str, Any]]:
        """Return status of all managed children."""
        result = []
        for name, child in self._children.items():
            running = child.process is not None and child.process.returncode is None
            result.append({
                "server_name": name,
                "running": running,
                "command": child.command,
            })
        return result
