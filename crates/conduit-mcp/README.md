# conduit-mcp

Model Context Protocol tool providers.

This crate connects to MCP servers, discovers the tools they expose, and adapts
each one into a Conduit `Tool`.

| Type | Role |
| --- | --- |
| `McpClient` | Connection lifecycle and JSON-RPC exchange |
| `McpSession` | One `initialize`-completed exchange |
| `McpTool` | One remote tool, as a Conduit `Tool` provider |

## Transports

`McpTransport` in a saved provider definition selects the transport:

| Variant | Transport | Framing |
| --- | --- | --- |
| `stdio` | Spawned child process | Newline-delimited JSON on stdin/stdout |
| `streamable_http` | POST to an endpoint | JSON body, or SSE `message` events |
| `sse` | GET stream plus POST endpoint | SSE `endpoint` event, then `message` events |

A transport only opens the channel. The MCP `initialize` handshake lives in the
session layer, so every transport reaches a server the same way.

## Connection Lifecycle

`McpClient` holds a transport factory, not a connection. Building one never
touches the network, so a client can be constructed from a provider definition
whether or not the server is running. Each request opens a fresh session,
performs the handshake, exchanges one message, and closes.

Requests are id-matched: responses answering a different id are skipped rather
than mistaken for the reply. A single exchange is abandoned after 30 seconds.

## Tools

`McpTool` keeps the Conduit registration name and the tool's real MCP name
separate. `Provider::name` returns the registration name a pipeline graph node
refers to, while `spec().name` returns the name the model must call — a server
definition may register tools under aliases, but the model still sees the real
names.

`invoke` flattens the server's `content` array into text so the model sees tool
output as prose. Results carrying no text content are returned unchanged so no
data is lost.

## Health

`health()` lists the server's tools. It reports `Healthy` when the handshake and
listing succeed and `Unhealthy` with the failure reason otherwise, which is the
narrowest non-destructive check available — it never invokes a tool.
