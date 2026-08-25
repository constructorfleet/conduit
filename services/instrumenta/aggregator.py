"""HTTP upstream MCP aggregation.

At boot Instrumenta reads every enabled HTTP upstream from the backend,
connects to each via the `mcp` SDK's streamable-HTTP client, lists their
tools/prompts/resources, and re-registers them on Instrumenta's own
`MCPServer` under a `<server_name>.<item_name>` prefix so nothing collides
with the built-ins.

Live config changes (add/remove servers via the CRUD endpoints) do NOT
mutate the aggregated surface in v1 — the operator restarts Instrumenta to
pick up new upstreams. This keeps the aggregator simple and matches Conduit's
own snapshot-once posture (see wayfinder decision #204). A follow-up PR can
add hot-reload once demand exists.

Filter-on-unreachable is deferred (decision #204): items from an unreachable
upstream stay advertised; the call fails loud with the upstream's error.
"""

from __future__ import annotations

import logging
from contextlib import AsyncExitStack
from dataclasses import dataclass, field
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
    prompt_count: int = 0
    resource_count: int = 0
    last_error: str | None = None


@dataclass
class UpstreamPrompts:
    """Cached prompt metadata from an upstream."""

    server_name: str
    prompts: list[types.Prompt] = field(default_factory=list)


@dataclass
class UpstreamResources:
    """Cached resource metadata from an upstream."""

    server_name: str
    resources: list[types.Resource] = field(default_factory=list)


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
        self._upstream_prompts: dict[str, UpstreamPrompts] = {}
        self._upstream_resources: dict[str, UpstreamResources] = {}

    @staticmethod
    def _default_client_factory(server: UpstreamServer) -> Client:
        assert server.url is not None
        return Client(server.url, raise_exceptions=True)

    async def start(self, mcp_server: MCPServer) -> None:
        """Connect to every enabled HTTP upstream, register its tools/prompts/resources."""
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
                )
                continue
            if server.transport != "http":
                # stdio is handled by the stdio supervisor; log and skip.
                LOG.warning(
                    "upstream %s uses transport=%s; skipping HTTP aggregation",
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
            listed_tools = await client.list_tools()
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

        # Register tools.
        for tool in listed_tools.tools:
            self._register_forwarding_tool(server, tool, client, mcp_server)

        # List prompts and resources (best-effort; some upstreams may not support them).
        prompt_count = 0
        resource_count = 0
        try:
            listed_prompts = await client.list_prompts()
            if listed_prompts.prompts:
                self._upstream_prompts[server.id] = UpstreamPrompts(
                    server_name=server.name,
                    prompts=listed_prompts.prompts,
                )
                prompt_count = len(listed_prompts.prompts)
        except Exception as exc:  # noqa: BLE001
            LOG.debug("upstream %s has no prompts: %s", server.name, exc)

        try:
            listed_resources = await client.list_resources()
            if listed_resources.resources:
                self._upstream_resources[server.id] = UpstreamResources(
                    server_name=server.name,
                    resources=listed_resources.resources,
                )
                resource_count = len(listed_resources.resources)
        except Exception as exc:  # noqa: BLE001
            LOG.debug("upstream %s has no resources: %s", server.name, exc)

        self._statuses[server.id] = UpstreamStatus(
            id=server.id,
            name=server.name,
            url=server.url,
            enabled=True,
            reachable=True,
            tool_count=len(listed_tools.tools),
            prompt_count=prompt_count,
            resource_count=resource_count,
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

    def client_for(self, server_id: str) -> Client | None:
        """Return the MCP client for a given upstream, or None."""
        return self._clients.get(server_id)

    def upstream_prompts(self) -> list[tuple[str, types.Prompt]]:
        """All upstream prompts as (server_name, prompt) pairs."""
        result = []
        for up in self._upstream_prompts.values():
            for p in up.prompts:
                result.append((up.server_name, p))
        return result

    def upstream_resources(self) -> list[tuple[str, types.Resource]]:
        """All upstream resources as (server_name, resource) pairs."""
        result = []
        for ur in self._upstream_resources.values():
            for r in ur.resources:
                result.append((ur.server_name, r))
        return result

    def statuses(self) -> list[UpstreamStatus]:
        return list(self._statuses.values())

    async def close(self) -> None:
        if self._exit_stack is not None:
            await self._exit_stack.__aexit__(None, None, None)
            self._exit_stack = None
