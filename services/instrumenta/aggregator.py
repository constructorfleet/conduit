"""HTTP upstream MCP aggregation.

At boot Instrumenta reads every enabled HTTP upstream from the backend,
connects to each via the `mcp` SDK's streamable-HTTP client, lists their
tools, and re-registers them on Instrumenta's own `MCPServer` under a
`<server_name>.<tool_name>` prefix so nothing collides with the built-ins.

Live config changes (add/remove servers via the CRUD endpoints) do NOT
mutate the aggregated tool set in v1 — the operator restarts Instrumenta to
pick up new upstreams. This keeps the aggregator simple and matches Conduit's
own snapshot-once posture (see wayfinder decision #204). A follow-up PR can
add hot-reload once demand exists.

Filter-on-unreachable is deferred (decision #204): tools from an unreachable
upstream stay advertised; the call fails loud with the upstream's error.
"""

from __future__ import annotations

import logging
from contextlib import AsyncExitStack
from dataclasses import dataclass
from typing import Any, Callable

from mcp import types
from mcp.client import Client
from mcp.server.mcpserver import MCPServer

from .backend import Backend, UpstreamServer
from .secret_box import SecretBox

LOG = logging.getLogger("instrumenta.aggregator")


@dataclass
class UpstreamStatus:
    """Per-upstream reachability snapshot for `/upstreams`."""

    id: str
    name: str
    url: str | None
    enabled: bool
    reachable: bool
    tool_count: int
    last_error: str | None


class Aggregator:
    """Owns MCP client sessions for enabled HTTP upstreams.

    Held on `app.state.aggregator`; its `start()` is called from the FastAPI
    lifespan, its `close()` from teardown. `attach_to_mcp_server()` registers
    upstream tools on the local MCP server before requests start arriving.
    """

    def __init__(
        self,
        backend: Backend,
        secret_box: SecretBox,
        client_factory: Callable[[UpstreamServer], Client] | None = None,
    ):
        """`client_factory` is injectable so tests can substitute an in-memory
        transport; production callers omit it and get a URL-based streamable-
        HTTP `Client`.
        """
        self.backend = backend
        self.secret_box = secret_box
        self._client_factory = client_factory or self._default_client_factory
        self._exit_stack: AsyncExitStack | None = None
        self._statuses: dict[str, UpstreamStatus] = {}
        self._clients: dict[str, Client] = {}

    @staticmethod
    def _default_client_factory(server: UpstreamServer) -> Client:
        assert server.url is not None
        return Client(server.url, raise_exceptions=True)

    async def start(self, mcp_server: MCPServer) -> None:
        """Connect to every enabled HTTP upstream, register its tools."""
        self._exit_stack = AsyncExitStack()
        await self._exit_stack.__aenter__()

        for server in self.backend.list_upstream_servers():
            if not server.enabled:
                self._statuses[server.id] = UpstreamStatus(
                    id=server.id,
                    name=server.name,
                    url=server.url,
                    enabled=False,
                    reachable=False,
                    tool_count=0,
                    last_error=None,
                )
                continue
            if server.transport != "http":
                # stdio is a later slice; log and skip.
                LOG.warning(
                    "upstream %s uses transport=%s; skipping (v1 is HTTP-only)",
                    server.name,
                    server.transport,
                )
                continue
            await self._attach_http_upstream(server, mcp_server)

    async def _attach_http_upstream(
        self, server: UpstreamServer, mcp_server: MCPServer
    ) -> None:
        assert self._exit_stack is not None
        try:
            client = self._client_factory(server)
            await self._exit_stack.enter_async_context(client)
            listed = await client.list_tools()
        except Exception as exc:  # noqa: BLE001 — surface any client error
            LOG.warning("upstream %s unreachable: %s", server.name, exc)
            self._statuses[server.id] = UpstreamStatus(
                id=server.id,
                name=server.name,
                url=server.url,
                enabled=True,
                reachable=False,
                tool_count=0,
                last_error=str(exc),
            )
            return

        self._clients[server.id] = client
        for tool in listed.tools:
            self._register_forwarding_tool(server, tool, client, mcp_server)

        self._statuses[server.id] = UpstreamStatus(
            id=server.id,
            name=server.name,
            url=server.url,
            enabled=True,
            reachable=True,
            tool_count=len(listed.tools),
            last_error=None,
        )

    def _register_forwarding_tool(
        self,
        server: UpstreamServer,
        tool: types.Tool,
        client: Client,
        mcp_server: MCPServer,
    ) -> None:
        prefixed_name = f"{server.name}.{tool.name}"

        async def forward(**kwargs: Any) -> Any:
            result = await client.call_tool(tool.name, kwargs)
            # Return the raw content list; the SDK wraps it appropriately on
            # the outbound side.
            return result.content

        forward.__name__ = tool.name  # keep introspection sane
        mcp_server.add_tool(
            forward,
            name=prefixed_name,
            description=tool.description or f"Forwarded from {server.name}",
        )

    def statuses(self) -> list[UpstreamStatus]:
        return list(self._statuses.values())

    async def close(self) -> None:
        if self._exit_stack is not None:
            await self._exit_stack.__aexit__(None, None, None)
            self._exit_stack = None
