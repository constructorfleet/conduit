"""Upstream MCP aggregation (HTTP and stdio).

At boot Instrumenta reads every enabled upstream from the backend, connects
to each via the `mcp` SDK — the streamable-HTTP client for `http` upstreams,
the stdio client transport for `stdio` upstreams — lists their
tools/prompts/resources, and re-registers them on Instrumenta's own
`MCPServer` under a `<server_name>.<item_name>` prefix so nothing collides
with the built-ins.

Both transports are handed to an `UpstreamSupervisor`, one per transport,
which holds each connection open with autorestart; forwarding tools are
registered the first time a connection succeeds — including a retry after a
lost boot race — so a transient failure never permanently hides them.

Live config changes (add/remove servers via the CRUD endpoints) do NOT
mutate the aggregated surface in v1 — the operator restarts Instrumenta to
pick up new upstreams. This keeps the aggregator simple and matches Conduit's
own snapshot-once posture (see wayfinder decision #204). A follow-up PR can
add hot-reload once demand exists.

Filter-on-unreachable holds for both transports now — stdio in #274, HTTP in
#252. An upstream that goes away has its forwarding tools removed and gets
them back on reconnect, because a tool that cannot be called should not be
advertised. This supersedes the deferral in decision #204, which was taken
when HTTP upstreams were attached once and never re-probed.

The same poll keeps the tool set current (#282): an upstream that adds,
removes or renames a tool while staying reachable has its forwarders replaced
rather than going on advertising the set it had at attach time.
"""

from __future__ import annotations

import asyncio
import inspect
import keyword
import logging
import re
from dataclasses import dataclass, field
from typing import Any, Callable

from mcp import types
from mcp.client import Client
from mcp.server.mcpserver import MCPServer

from .backend import Backend, UpstreamServer
from .secret_box import SecretBox
from .supervisor import ClientHolder, UpstreamSupervisor, http_connect, stdio_connect

LOG = logging.getLogger("instrumenta.aggregator")

#: How often a reachable HTTP upstream is re-listed, for liveness and to pick
#: up a changed tool set. Slower than the stdio poll on purpose: a stdio child
#: is a local process this service owns, while an HTTP upstream is someone
#: else's server and every poll is a request to it.
HTTP_LIVENESS_POLL = 30.0


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
        #: Rows for upstreams nothing supervises -- disabled, or a transport
        #: this build does not speak. Supervised rows come from the
        #: supervisors themselves, which own the live truth about them.
        self._statuses: dict[str, UpstreamStatus] = {}
        self._upstream_prompts: dict[str, UpstreamPrompts] = {}
        self._upstream_resources: dict[str, UpstreamResources] = {}
        self._stdio = UpstreamSupervisor(
            self._register_supervised_tools,
            connect=stdio_connect,
            unregister_tools=self._unregister_supervised_tools,
        )
        # HTTP upstreams run the same loop. They used to be attached once and
        # never contacted again, which meant a dead one kept its tools
        # advertised (#252) and a live one that changed its tools went on
        # advertising the attach-time set (#282).
        self._http = UpstreamSupervisor(
            self._register_supervised_tools,
            connect=client_factory or http_connect,
            unregister_tools=self._unregister_supervised_tools,
            on_connected=self._refresh_upstream_items,
            liveness_poll=HTTP_LIVENESS_POLL,
        )
        self._mcp_server: MCPServer | None = None
        #: Prefixed tool names currently registered for each supervised
        #: upstream, so a removal only ever touches names this aggregator
        #: added.
        self._registered_tool_names: dict[str, list[str]] = {}

    async def start(self, mcp_server: MCPServer) -> None:
        """Supervise every enabled upstream and register what it offers."""
        self._mcp_server = mcp_server

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
                self._stdio.add(server)
                continue
            if server.transport == "http":
                self._http.add(server)
                continue
            LOG.warning(
                "upstream %s uses unknown transport=%s; skipping",
                server.name,
                server.transport,
            )

        # Both boot windows run together: one slow upstream should not be
        # added to the wait for the other transport's.
        await asyncio.gather(self._http.start(), self._stdio.start())

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

    def _register_supervised_tools(
        self,
        server: UpstreamServer,
        tools: list[types.Tool],
        holder: ClientHolder,
    ) -> None:
        """Supervisor callback: register an upstream's forwarding tools.

        Called on each connection that succeeds while the upstream has no
        tools registered -- the first one, a retry after a failed boot race,
        a reconnect after it died -- and again when a reachable upstream
        changes what it offers, so the surface follows the upstream without an
        Instrumenta restart. Forwarders read the live client from `holder`,
        which the supervisor swaps in place across reconnects.
        """
        assert self._mcp_server is not None
        registered = []
        for tool in tools:
            registered.append(
                self._register_forwarding_tool(
                    server, tool, lambda h=holder: h.client, self._mcp_server
                )
            )
        self._registered_tool_names[server.id] = registered

    def _unregister_supervised_tools(
        self, server: UpstreamServer, _tools: list[types.Tool]
    ) -> None:
        """Supervisor callback: take an upstream's tools off the surface.

        Removes the names recorded at registration rather than re-deriving
        them from the tool list, because the list may describe either the
        upstream that has gone away or the surface that is being replaced --
        and what was registered is the only thing safe to remove.
        """
        assert self._mcp_server is not None
        for name in self._registered_tool_names.pop(server.id, []):
            try:
                self._mcp_server.remove_tool(name)
            except Exception as exc:  # noqa: BLE001 — removal is best-effort
                LOG.debug("tool %s was already gone: %s", name, exc)

    async def _refresh_upstream_items(self, server: UpstreamServer, client: Client) -> None:
        """Supervisor callback: re-read an HTTP upstream's prompts and resources.

        Best-effort, because plenty of upstreams serve neither. Runs on every
        successful connect rather than only the first, so a reconnect picks up
        whatever changed while the upstream was away -- the same reason the
        tool surface is re-synced.
        """
        try:
            listed = await client.list_prompts()
            if listed.prompts:
                self._upstream_prompts[server.id] = UpstreamPrompts(
                    server_name=server.name, prompts=listed.prompts
                )
            else:
                self._upstream_prompts.pop(server.id, None)
        except Exception as exc:  # noqa: BLE001
            LOG.debug("upstream %s has no prompts: %s", server.name, exc)

        try:
            listed_resources = await client.list_resources()
            if listed_resources.resources:
                self._upstream_resources[server.id] = UpstreamResources(
                    server_name=server.name, resources=listed_resources.resources
                )
            else:
                self._upstream_resources.pop(server.id, None)
        except Exception as exc:  # noqa: BLE001
            LOG.debug("upstream %s has no resources: %s", server.name, exc)

    def client_for(self, server_id: str) -> Client | None:
        """Return the live MCP client for an upstream, or None while it is down.

        Read from the supervisor's holder rather than a map of our own: a
        reconnect swaps the client in place, and a second copy of that
        reference is a second thing to keep in step.
        """
        for supervisor in (self._http, self._stdio):
            client = supervisor.client_for(server_id)
            if client is not None:
                return client
        return None

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
        for supervisor in (self._http, self._stdio):
            for row in supervisor.statuses():
                server_id = row["id"]
                rows.append(
                    UpstreamStatus(
                        id=server_id,
                        name=row["name"],
                        url=row["url"],
                        enabled=row["enabled"],
                        reachable=row["reachable"],
                        tool_count=row["tool_count"],
                        prompt_count=len(
                            self._upstream_prompts[server_id].prompts
                            if server_id in self._upstream_prompts
                            else []
                        ),
                        resource_count=len(
                            self._upstream_resources[server_id].resources
                            if server_id in self._upstream_resources
                            else []
                        ),
                        last_error=row["last_error"],
                    )
                )
        return rows

    async def close(self) -> None:
        await asyncio.gather(self._http.close(), self._stdio.close())
