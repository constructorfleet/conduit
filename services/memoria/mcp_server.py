"""MCP server implementation for Memoria."""

import asyncio
import json
import logging
from typing import Any

import httpx

LOG = logging.getLogger("memoria.mcp")


class MCPServer:
    """Model Context Protocol server for Memoria."""

    def __init__(self):
        self.base_url = os.getenv("MEMORIA_BASE_URL", "http://localhost:8080")
        self.api_key = os.getenv("MEMORIA_API_KEY")
        self.server_name = os.getenv("MEMORIA_MCP_SERVER_NAME", "memoria")
        self.client: httpx.AsyncClient | None = None

    async def initialize_client(self) -> None:
        """Initialize HTTP client."""
        if self.client is None:
            headers = {}
            if self.api_key:
                headers["Authorization"] = f"Bearer {self.api_key}"

            self.client = httpx.AsyncClient(
                base_url=self.base_url,
                headers=headers,
                timeout=30.0,
            )

    async def close(self) -> None:
        """Close HTTP client."""
        if self.client:
            await self.client.aclose()
            self.client = None

    async def run(self, transport: str = "stdio") -> None:
        """Run MCP server with specified transport."""
        await self.initialize_client()

        if transport == "stdio":
            await self._run_stdio()
        elif transport == "http":
            await self._run_http()
        elif transport == "sse":
            await self._run_sse()
        else:
            raise ValueError(f"Unsupported transport: {transport}")

    async def _run_stdio(self) -> None:
        """Run MCP server over stdio."""
        import sys

        LOG.info("Starting MCP server over stdio")

        while True:
            try:
                line = sys.stdin.readline()
                if not line:
                    break

                request = json.loads(line.strip())
                response = await self._handle_request(request)
                print(json.dumps(response))
                sys.stdout.flush()

            except json.JSONDecodeError as e:
                LOG.error(f"JSON decode error: {e}")
                self._send_error(-32700, "Parse error")
            except Exception as e:
                LOG.error(f"Error handling request: {e}")
                self._send_error(-32603, "Internal error")

    async def _run_http(self) -> None:
        """Run MCP server over HTTP."""
        from fastapi import FastAPI
        import uvicorn

        LOG.info("Starting MCP server over HTTP")

        app = FastAPI(title=f"{self.server_name} MCP Server")

        @app.post("/")
        async def handle_http(request: dict[str, Any]):
            """Handle HTTP MCP requests."""
            return await self._handle_request(request)

        uvicorn.run(app, host="0.0.0.0", port=8081)

    async def _run_sse(self) -> None:
        """Run MCP server over Server-Sent Events."""
        from fastapi import FastAPI
        from fastapi.responses import StreamingResponse
        import uvicorn

        LOG.info("Starting MCP server over SSE")

        app = FastAPI(title=f"{self.server_name} MCP Server")

        @app.get("/")
        async def sse_endpoint():
            """SSE endpoint for MCP communication."""
            async def event_stream():
                while True:
                    # Send keepalive comments
                    yield ": keepalive\n\n"
                    await asyncio.sleep(15)

            return StreamingResponse(event_stream(), media_type="text/event-stream")

        @app.post("/message")
        async def handle_sse_message(request: dict[str, Any]):
            """Handle SSE MCP messages."""
            return await self._handle_request(request)

        uvicorn.run(app, host="0.0.0.0", port=8081)

    async def _handle_request(self, request: dict[str, Any]) -> dict[str, Any]:
        """Handle MCP JSON-RPC request."""
        method = request.get("method")
        params = request.get("params", {})
        request_id = request.get("id")

        try:
            if method == "initialize":
                return self._initialize_response(request_id)
            elif method == "tools/list":
                return await self._tools_list_response(request_id)
            elif method == "tools/call":
                return await self._tools_call_response(request_id, params)
            elif method == "resources/list":
                return await self._resources_list_response(request_id)
            elif method == "resources/read":
                return await self._resources_read_response(request_id, params)
            else:
                return self._error_response(request_id, -32601, "Method not found")

        except Exception as e:
            LOG.error(f"Error handling {method}: {e}")
            return self._error_response(request_id, -32603, f"Internal error: {str(e)}")

    def _initialize_response(self, request_id: Any) -> dict[str, Any]:
        """Handle initialize request."""
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {
                        "listChanged": False,
                    },
                    "resources": {
                        "subscribe": False,
                        "listChanged": False,
                    },
                },
                "serverInfo": {
                    "name": self.server_name,
                    "version": "1.0.0",
                },
            },
        }

    async def _tools_list_response(self, request_id: Any) -> dict[str, Any]:
        """Handle tools/list request."""
        tools = [
            {
                "name": "store_engram",
                "description": "Store a new memory engram",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "The content of the memory",
                            "minLength": 1,
                            "maxLength": 10000,
                        },
                        "speaker_id": {
                            "type": "string",
                            "description": "Optional speaker identifier",
                        },
                        "conversation_id": {
                            "type": "string",
                            "description": "Optional conversation identifier",
                        },
                        "scope": {
                            "type": "string",
                            "enum": ["global", "conversation", "speaker"],
                            "description": "Memory scope",
                            "default": "global",
                        },
                        "metadata": {
                            "type": "object",
                            "description": "Optional metadata",
                        },
                    },
                    "required": ["content"],
                },
            },
            {
                "name": "search_engrams",
                "description": "Search memory engrams by content",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query",
                            "minLength": 1,
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results to return",
                            "default": 10,
                            "minimum": 1,
                            "maximum": 100,
                        },
                        "scope": {
                            "type": "string",
                            "enum": ["global", "conversation", "speaker"],
                            "description": "Filter by scope",
                        },
                        "speaker_id": {
                            "type": "string",
                            "description": "Filter by speaker ID",
                        },
                        "conversation_id": {
                            "type": "string",
                            "description": "Filter by conversation ID",
                        },
                    },
                    "required": ["query"],
                },
            },
            {
                "name": "get_engram",
                "description": "Retrieve a specific engram by ID",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "engram_id": {
                            "type": "string",
                            "description": "Engram identifier",
                        },
                    },
                    "required": ["engram_id"],
                },
            },
            {
                "name": "list_engrams",
                "description": "List engrams with optional filters",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "enum": ["global", "conversation", "speaker"],
                            "description": "Filter by scope",
                        },
                        "speaker_id": {
                            "type": "string",
                            "description": "Filter by speaker ID",
                        },
                        "conversation_id": {
                            "type": "string",
                            "description": "Filter by conversation ID",
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results to return",
                            "default": 100,
                            "minimum": 1,
                            "maximum": 1000,
                        },
                    },
                },
            },
            {
                "name": "delete_engram",
                "description": "Delete an engram by ID",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "engram_id": {
                            "type": "string",
                            "description": "Engram identifier",
                        },
                    },
                    "required": ["engram_id"],
                },
            },
            {
                "name": "update_engram",
                "description": "Update an existing engram",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "engram_id": {
                            "type": "string",
                            "description": "Engram identifier",
                        },
                        "content": {
                            "type": "string",
                            "description": "New content for the engram",
                            "minLength": 1,
                            "maxLength": 10000,
                        },
                        "metadata": {
                            "type": "object",
                            "description": "New metadata for the engram",
                        },
                    },
                    "required": ["engram_id"],
                },
            },
        ]

        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"tools": tools},
        }

    async def _tools_call_response(self, request_id: Any, params: dict[str, Any]) -> dict[str, Any]:
        """Handle tools/call request."""
        tool_name = params.get("name")
        arguments = params.get("arguments", {})

        if tool_name == "store_engram":
            result = await self._store_engram(arguments)
        elif tool_name == "search_engrams":
            result = await self._search_engrams(arguments)
        elif tool_name == "get_engram":
            result = await self._get_engram(arguments)
        elif tool_name == "list_engrams":
            result = await self._list_engrams(arguments)
        elif tool_name == "delete_engram":
            result = await self._delete_engram(arguments)
        elif tool_name == "update_engram":
            result = await self._update_engram(arguments)
        else:
            return self._error_response(request_id, -32601, f"Unknown tool: {tool_name}")

        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": json.dumps(result, indent=2),
                    }
                ]
            },
        }

    async def _resources_list_response(self, request_id: Any) -> dict[str, Any]:
        """Handle resources/list request."""
        resources = [
            {
                "uri": "engrams://",
                "name": "All Engrams",
                "description": "All stored memory engrams",
                "mimeType": "application/json",
            },
            {
                "uri": "engrams://scope/global",
                "name": "Global Engrams",
                "description": "Global scope memory engrams",
                "mimeType": "application/json",
            },
            {
                "uri": "engrams://scope/conversation",
                "name": "Conversation Engrams",
                "description": "Conversation scope memory engrams",
                "mimeType": "application/json",
            },
            {
                "uri": "engrams://scope/speaker",
                "name": "Speaker Engrams",
                "description": "Speaker scope memory engrams",
                "mimeType": "application/json",
            },
        ]

        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"resources": resources},
        }

    async def _resources_read_response(self, request_id: Any, params: dict[str, Any]) -> dict[str, Any]:
        """Handle resources/read request."""
        uri = params.get("uri", "")

        if uri == "engrams://":
            engrams = await self._list_engrams({"limit": 1000})
        elif uri.startswith("engrams://scope/"):
            scope = uri.split("/")[-1]
            engrams = await self._list_engrams({"scope": scope, "limit": 1000})
        else:
            return self._error_response(request_id, -32602, f"Unknown resource URI: {uri}")

        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "contents": [
                    {
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": json.dumps(engrams, indent=2),
                    }
                ]
            },
        }

    async def _store_engram(self, arguments: dict[str, Any]) -> dict[str, Any]:
        """Store a new engram via HTTP API."""
        if not self.client:
            await self.initialize_client()

        response = await self.client.post("/engrams", json=arguments)
        response.raise_for_status()
        return response.json()

    async def _search_engrams(self, arguments: dict[str, Any]) -> dict[str, Any]:
        """Search engrams via HTTP API."""
        if not self.client:
            await self.initialize_client()

        response = await self.client.post("/engrams/search", json=arguments)
        response.raise_for_status()
        return response.json()

    async def _get_engram(self, arguments: dict[str, Any]) -> dict[str, Any]:
        """Get an engram via HTTP API."""
        if not self.client:
            await self.initialize_client()

        engram_id = arguments.get("engram_id")
        response = await self.client.get(f"/engrams/{engram_id}")
        response.raise_for_status()
        return response.json()

    async def _list_engrams(self, arguments: dict[str, Any]) -> dict[str, Any]:
        """List engrams via HTTP API."""
        if not self.client:
            await self.initialize_client()

        response = await self.client.get("/engrams", params=arguments)
        response.raise_for_status()
        return response.json()

    async def _delete_engram(self, arguments: dict[str, Any]) -> dict[str, Any]:
        """Delete an engram via HTTP API."""
        if not self.client:
            await self.initialize_client()

        engram_id = arguments.get("engram_id")
        response = await self.client.delete(f"/engrams/{engram_id}")
        response.raise_for_status()
        return {"success": True}

    async def _update_engram(self, arguments: dict[str, Any]) -> dict[str, Any]:
        """Update an engram via HTTP API."""
        if not self.client:
            await self.initialize_client()

        engram_id = arguments.pop("engram_id")
        response = await self.client.patch(f"/engrams/{engram_id}", json=arguments)
        response.raise_for_status()
        return response.json()

    def _error_response(self, request_id: Any, code: int, message: str) -> dict[str, Any]:
        """Create error response."""
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {
                "code": code,
                "message": message,
            },
        }

    def _send_error(self, code: int, message: str) -> None:
        """Send error response over stdio."""
        import sys

        response = self._error_response(None, code, message)
        print(json.dumps(response))
        sys.stdout.flush()


import os