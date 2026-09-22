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

Filter-on-unreachable now holds for stdio upstreams (#274): a child that dies
has its forwarding tools removed and gets them back on reconnect, because a
tool that cannot be called should not be advertised.

It does not yet hold for HTTP upstreams. Those are attached once at `start()`
and never re-probed, so an HTTP upstream that goes away after boot keeps its
items advertised and the call fails loud with the upstream's error -- the
posture decision #204 describes. Closing that gap means giving HTTP upstreams
the liveness polling stdio already has.
"""

from __future__ import annotations

import asyncio
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
        self._stdio = StdioSupervisor(
            self._register_stdio_tools, unregister_tools=self._unregister_stdio_tools
        )
        self._stdio_mcp_server: MCPServer | None = None
        #: Prefixed tool names currently registered for each stdio upstream,
        #: so a removal only ever touches names this aggregator added.
        self._stdio_tool_names: dict[str, list[str]] = {}

    #: Seconds to wait for an upstream that has no `timeout_seconds` of its own
    #: before giving up on the attach. Not a read timeout -- it only bounds the
    #: connect-and-list at boot, so an upstream that accepts the connection and
    #: then goes quiet cannot hold up every other upstream behind it.
    DEFAULT_ATTACH_TIMEOUT = 30.0

    @staticmethod
    def _default_client_factory(server: UpstreamServer) -> Client:
        assert server.url is not None
        # The row's timeout is what the operator asked for; dropping it left
        # reads from a hung upstream unbounded.
        return Client(
            server.url,
            raise_exceptions=True,
            read_timeout_seconds=(
                None if server.timeout_seconds is None else float(server.timeout_seconds)
            ),
        )

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
        # `read_timeout_seconds` only bounds reads once a session exists. An
        # upstream that accepts the connection and never answers would hang
        # the attach itself, and `start()` runs these one after another, so
        # one hung upstream would stall the whole boot.
        budget = float(server.timeout_seconds or self.DEFAULT_ATTACH_TIMEOUT)
        client = None
        try:
            client = self._client_factory(server)
            async with asyncio.timeout(budget):
                await self._exit_stack.enter_async_context(client)
                listed_tools = await client.list_tools()
        except TimeoutError:
            LOG.warning(
                "upstream %s did not answer within %ss; detaching", server.name, budget
            )
            await self._discard_client(server, client)
            self._statuses[server.id] = UpstreamStatus(
                id=server.id,
                name=server.name,
                url=server.url,
                enabled=True,
                reachable=False,
                tool_count=0,
                last_error=f"timed out after {budget:g}s",
            )
            return
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

    @staticmethod
    async def _discard_client(server: UpstreamServer, client: Client | None) -> None:
        """Close a client whose attach timed out.

        The exit stack only owns what `enter_async_context` returned, and a
        timeout means it may not have returned, so the half-open client is
        closed here or it leaks for the life of the process. Best-effort: a
        client that hung on connect may well hang on close, so the shutdown
        gets its own short bound.
        """
        if client is None:
            return
        try:
            async with asyncio.timeout(5):
                await client.__aexit__(None, None, None)
        except Exception as exc:  # noqa: BLE001 -- teardown must not mask the timeout
            LOG.debug("discarding hung upstream %s raised: %s", server.name, exc)

    def _register_forwarding_tool(
        self,
        server: UpstreamServer,
        tool: types.Tool,
        get_client: Callable[[], Client | None],
        mcp_server: MCPServer,
    ) -> str:
        """Register one forwarding tool; returns the prefixed name it took."""
        prefixed_name = f"{server.name}.{tool.name}"
        forward = build_forwarder(
            get_client, tool.name, server.name, getattr(tool, "input_schema", None)
        )
        mcp_server.add_tool(
            forward,
            name=prefixed_name,
            description=tool.description or f"Forwarded from {server.name}",
        )
        return prefixed_name

    def _register_stdio_tools(
        self,
        server: UpstreamServer,
        tools: list[types.Tool],
        holder: ClientHolder,
    ) -> None:
        """Supervisor callback: register a stdio upstream's forwarding tools.

        Called on each connection that succeeds while the upstream has no
        tools registered -- the first one, a retry after a failed boot race,
        or a reconnect after the child died -- so tools appear without an
        Instrumenta restart. Forwarders read the live client from `holder`,
        which the supervisor swaps in place across reconnects.
        """
        assert self._stdio_mcp_server is not None
        registered = []
        for tool in tools:
            registered.append(
                self._register_forwarding_tool(
                    server, tool, lambda h=holder: h.client, self._stdio_mcp_server
                )
            )
        self._stdio_tool_names[server.id] = registered

    def _unregister_stdio_tools(
        self, server: UpstreamServer, _tools: list[types.Tool]
    ) -> None:
        """Supervisor callback: take a dead upstream's tools off the surface.

        Removes the names recorded at registration rather than re-deriving
        them from the tool list, because the list came from the child and the
        child is the thing that has gone away.
        """
        assert self._stdio_mcp_server is not None
        for name in self._stdio_tool_names.pop(server.id, []):
            try:
                self._stdio_mcp_server.remove_tool(name)
            except Exception as exc:  # noqa: BLE001 — removal is best-effort
                LOG.debug("tool %s was already gone: %s", name, exc)

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
