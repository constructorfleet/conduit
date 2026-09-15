"""Upstream MCP aggregation (HTTP and stdio).

At boot Instrumenta reads every enabled upstream from the backend, connects
to each via the `mcp` SDK — the streamable-HTTP client for `http` upstreams,
the stdio client transport for `stdio` upstreams — lists their
tools/prompts/resources, and re-registers them on Instrumenta's own
`MCPServer` under a `<server_name>.<item_name>` prefix so nothing collides
with the built-ins.

HTTP upstreams are connected once, synchronously, at `start()`. stdio
upstreams are handed to `StdioSupervisor`, which spawns and supervises each
child with autorestart; their forwarding tools are registered the first time
a connection succeeds — including a retry after a lost boot race — so a
transient child failure never permanently hides its tools.

Live config changes (add/remove servers via the CRUD endpoints) do NOT
mutate the aggregated surface in v1 — the operator restarts Instrumenta to
pick up new upstreams. This keeps the aggregator simple and matches Conduit's
own snapshot-once posture (see wayfinder decision #204). A follow-up PR can
add hot-reload once demand exists.

Filter-on-unreachable is deferred (decision #204): items from an unreachable
upstream stay advertised; the call fails loud with the upstream's error.
"""

from __future__ import annotations

import inspect
import keyword
import logging
import re
from contextlib import AsyncExitStack
from dataclasses import dataclass, field
from typing import Any, Callable

from mcp import types
from mcp.client import Client
from mcp.server.mcpserver import MCPServer

from .backend import Backend, UpstreamServer
from .secret_box import SecretBox
from .stdio_supervisor import ClientHolder, StdioSupervisor

LOG = logging.getLogger("instrumenta.aggregator")


def _safe_param_names(properties: dict[str, Any]) -> dict[str, str]:
    """Map each upstream property name to a valid, unique Python parameter name.

    A property whose name is already a valid, non-keyword identifier keeps it.
    One that is not (a hyphen, a leading digit, a Python keyword) gets a
    generated alias. The alias is chosen to be disjoint from *every* real
    property name and from every alias already assigned, so a generated alias
    can never shadow a property literally named like the alias (e.g. a real
    `arg_1` alongside a hyphenated name at that index) and silently drop a
    parameter. Returns an ordered mapping of parameter-name -> real-name.
    """
    real_names = list(properties.keys())
    real_set = set(real_names)
    used: set[str] = set()
    mapping: dict[str, str] = {}
    for real in real_names:
        if real.isidentifier() and not keyword.iskeyword(real):
            pname = real
        else:
            base = re.sub(r"\W", "_", real) or "arg"
            if not (base[0].isalpha() or base[0] == "_"):
                base = "_" + base
            candidate = base
            suffix = 0
            while (
                candidate in real_set
                or candidate in used
                or keyword.iskeyword(candidate)
                or not candidate.isidentifier()
            ):
                suffix += 1
                candidate = f"{base}_{suffix}"
            pname = candidate
        used.add(pname)
        mapping[pname] = real
    return mapping


def build_forwarder(
    get_client: Callable[[], "Client | None"],
    tool_name: str,
    server_name: str,
    input_schema: dict[str, Any] | None,
) -> Callable[..., Any]:
    """Build a forwarding coroutine that mirrors an upstream tool's parameters.

    The returned coroutine carries a synthesized ``__signature__`` derived from
    the upstream tool's input schema, so Instrumenta's own MCP server advertises
    and validates the real parameters rather than a single opaque ``kwargs``
    bag. Optional arguments left unset are dropped before forwarding so the
    upstream applies its own defaults. Non-identifier property names are mapped
    through collision-safe aliases (see `_safe_param_names`).
    """
    schema = input_schema if isinstance(input_schema, dict) else {}
    properties = schema.get("properties") or {}
    required = set(schema.get("required") or [])
    param_to_real = _safe_param_names(properties)
    optional_params = {p for p, real in param_to_real.items() if real not in required}

    params = [
        inspect.Parameter(
            pname,
            inspect.Parameter.KEYWORD_ONLY,
            annotation=Any,
            default=(inspect.Parameter.empty if real in required else None),
        )
        for pname, real in param_to_real.items()
    ]

    async def forward(**kwargs: Any) -> Any:
        client = get_client()
        if client is None:
            # A supervised upstream mid-reconnect has no live client. Fail loud
            # rather than call a dead transport.
            raise RuntimeError(f"upstream {server_name!r} is not currently connected")
        forwarded: dict[str, Any] = {}
        for pname, value in kwargs.items():
            if pname in optional_params and value is None:
                continue  # let the upstream fill its own default
            forwarded[param_to_real.get(pname, pname)] = value
        result = await client.call_tool(tool_name, forwarded)
        # Return the raw content list; the SDK wraps it appropriately on the
        # outbound side.
        return result.content

    forward.__name__ = tool_name if tool_name.isidentifier() else "forward"
    if params:
        forward.__signature__ = inspect.Signature(params)  # type: ignore[attr-defined]
    return forward


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
        self._stdio = StdioSupervisor(self._register_stdio_tools)
        self._stdio_mcp_server: MCPServer | None = None

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
            if server.transport == "stdio":
                # stdio upstreams are supervised out-of-band: the supervisor
                # spawns the child, and register-on-connect (below) adds the
                # forwarding tools whenever the first connection succeeds.
                self._stdio.add(server)
                continue
            if server.transport != "http":
                LOG.warning(
                    "upstream %s uses unknown transport=%s; skipping",
                    server.name,
                    server.transport,
                )
                continue
            await self._attach_http_upstream(server, mcp_server)

        self._stdio_mcp_server = mcp_server
        await self._stdio.start()

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

        # Register tools. The HTTP client is fixed for the process lifetime, so
        # the getter just returns it.
        for tool in listed_tools.tools:
            self._register_forwarding_tool(server, tool, lambda c=client: c, mcp_server)

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
        get_client: Callable[[], Client | None],
        mcp_server: MCPServer,
    ) -> None:
        prefixed_name = f"{server.name}.{tool.name}"
        forward = build_forwarder(
            get_client, tool.name, server.name, getattr(tool, "input_schema", None)
        )
        mcp_server.add_tool(
            forward,
            name=prefixed_name,
            description=tool.description or f"Forwarded from {server.name}",
        )

    def _register_stdio_tools(
        self,
        server: UpstreamServer,
        tools: list[types.Tool],
        holder: ClientHolder,
    ) -> None:
        """Supervisor callback: register a stdio upstream's forwarding tools.

        Called on the first successful connection (which may be a retry after
        a failed boot-race attempt), so tools appear without an Instrumenta
        restart. Forwarders read the live client from `holder`, which the
        supervisor swaps in place across reconnects.
        """
        assert self._stdio_mcp_server is not None
        for tool in tools:
            self._register_forwarding_tool(
                server, tool, lambda h=holder: h.client, self._stdio_mcp_server
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
        rows = list(self._statuses.values())
        for row in self._stdio.statuses():
            rows.append(
                UpstreamStatus(
                    id=row["id"],
                    name=row["name"],
                    url=row["url"],
                    enabled=row["enabled"],
                    reachable=row["reachable"],
                    tool_count=row["tool_count"],
                    last_error=row["last_error"],
                )
            )
        return rows

    async def close(self) -> None:
        await self._stdio.close()
        if self._exit_stack is not None:
            await self._exit_stack.__aexit__(None, None, None)
            self._exit_stack = None
