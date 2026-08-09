# Conduit Memoria

Conduit Memoria is the reference memory service that provides persistent storage and retrieval of conversation memories (engrams). It follows a hybrid architecture with both HTTP API and MCP server interfaces.

## Quick Start

```bash
docker compose --profile memoria up
```

Then either:

- open Memoria's UI at `http://memoria:8080/ui` (or through the Conduit Console's **Memoria** section) and click **Link to Conduit** — the two exchange keys and the provider definition is created for you; or
- create a `http_memory` Provider Definition whose `base_url` is `http://memoria:8080` by hand, and add a Memory stage to a pipeline.

## Architecture

### Dual Interface

**HTTP Interface** (Vox-style):
- FastAPI-based HTTP service
- Token-based authentication
- Self-contained HTML UI for memory management
- Vox-style linking with Conduit
- Background sync when linked

**MCP Interface** (Optional):
- Standard MCP server over stdio/HTTP/SSE
- Tools for memory operations
- Resources for memory access
- Seamless MCP client integration

### Storage Backends

| Backend | Search | Needs | Feature |
| --- | --- | --- | --- |
| `Builtin` | BM25 over unigrams | nothing at all | default |
| `PgVector` | cosine distance over embeddings | PostgreSQL with `pgvector` | optional |
| `Qdrant` | cosine distance over embeddings | Qdrant server | optional |

## The UI

Memoria ships a management UI at `/ui` (with `/` redirecting there). Features:

- **Dashboard** — service health, engram counts by scope, backend status
- **Engram Browser** — paginated list with filters (speaker, conversation, scope)
- **Engram Editor** — add/edit engrams with metadata
- **Search Interface** — both semantic and keyword search with results ranked by relevance
- **Speaker View** — all engrams for a specific speaker
- **Conversation View** — all engrams for a conversation
- **Link Panel** — Conduit integration status and controls
- **Bulk Operations** — batch delete, export

The UI is served without authentication so an operator with the key in their head can load the page and paste it in; every route it calls carries the bearer token. The token, if any, is taken from `?api_key=` on load and held only in memory for the tab's lifetime.

## The HTTP API

| Request | Body | Response |
| --- | --- | --- |
| `POST /engrams` | `{"content", "speaker_id"?, "conversation_id"?, "scope", "metadata"?}` | Created engram |
| `GET /engrams/{id}` | — | Engram or `404` |
| `PATCH /engrams/{id}` | `{"content"?, "metadata"?}` | Updated engram or `404` |
| `DELETE /engrams/{id}` | — | `204` or `404` |
| `GET /engrams` | `?scope=&speaker_id=&conversation_id=&limit=` | List of engrams |
| `POST /engrams/search` | `{"query", "limit"?, "scope"?, "speaker_id"?, "conversation_id"?}` | `[{"engram", "score"}]` |
| `GET /engrams/speakers/{speaker_id}` | — | Engrams for speaker |
| `GET /engrams/conversations/{conversation_id}` | — | Engrams for conversation |
| `GET /health` | — | Service health |
| `GET /link` | — | Link status |
| `POST /link` | `{"conduit_url", "operator_token", "peer_name"?, "force"?}` | Link status |
| `DELETE /link` | — | `204` after unlink |

## The MCP Interface

### Tools

- `store_engram(content, speaker_id?, conversation_id?, scope?, metadata?)` — Store a new engram
- `search_engrams(query, limit?, scope?, speaker_id?, conversation_id?)` — Search engrams
- `get_engram(engram_id)` — Retrieve specific engram
- `list_engrams(scope?, speaker_id?, conversation_id?, limit?)` — List engrams
- `delete_engram(engram_id)` — Delete an engram
- `update_engram(engram_id, content?, metadata?)` — Update engram

### Resources

- `engrams://` — All engrams
- `engrams://scope/global` — Global scope engrams
- `engrams://scope/conversation` — Conversation scope engrams
- `engrams://scope/speaker` — Speaker scope engrams

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `MEMORIA_BACKEND` | `builtin` | Storage backend: `builtin`, `pgvector`, or `qdrant` |
| `MEMORIA_API_KEY` | unset | Bearer token for API authentication |
| `MEMORIA_BASE_URL` | request base URL | Base URL Conduit should store for reaching Memoria |
| `MEMORIA_DATA_DIR` | `/data` | Storage directory for builtin backend |
| `MEMORIA_DATABASE_URL` | unset | PostgreSQL URL for pgvector backend |
| `MEMORIA_QDRANT_URL` | unset | Qdrant server URL for qdrant backend |
| `MEMORIA_QDRANT_COLLECTION` | `memoria` | Qdrant collection name |
| `MEMORIA_EMBEDDING_MODEL` | `sentence-transformers/all-MiniLM-L6-v2` | Embedding model for pgvector/qdrant |
| `MEMORIA_EMBEDDING_DEVICE` | `cpu` | Device for embeddings: `cpu` or `cuda` |
| `MEMORIA_CACHE_DIR` | `/cache` | Cache directory for models |
| `MEMORIA_MCP_ENABLED` | `false` | Enable MCP server |
| `MEMORIA_MCP_TRANSPORT` | `stdio` | MCP transport: `stdio`, `http`, or `sse` |
| `MEMORIA_MCP_SERVER_NAME` | `memoria` | MCP server name |
| `MEMORIA_CONDUIT_URL` | unset | Conduit URL for linking |
| `MEMORIA_SYNC_INTERVAL_SECONDS` | `300` | Sync interval when linked |
| `MEMORIA_SYNC_MAX_BACKOFF_SECONDS` | `900` | Max backoff for sync retries |
| `MEMORIA_SEARCH_LIMIT` | `10` | Default search result limit |
| `MEMORIA_SEARCH_TIMEOUT_MS` | `3000` | Search timeout in milliseconds |
| `MEMORIA_SIMILARITY_THRESHOLD` | `0.7` | Similarity threshold for semantic search |

## Storage

### Builtin Backend

JSON file storage with in-memory BM25 search:
- No external dependencies
- Suitable for small deployments (<10K engrams)
- Fast startup and simple deployment
- Persistent storage in single JSON file

### PgVector Backend

PostgreSQL with vector embeddings:
- Persistent database storage
- Vector embeddings for semantic search
- Scalable to millions of engrams
- Requires PostgreSQL with pgvector extension
- Embedding model downloaded on first use

## Images

```bash
# CPU - Builtin backend
docker build -t conduit-memoria:builtin services/memoria

# CPU - PgVector backend
docker build -t conduit-memoria:pgvector \
  --build-arg BACKEND=pgvector services/memoria

# CPU - Qdrant backend
docker build -t conduit-memoria:qdrant \
  --build-arg BACKEND=qdrant services/memoria

# GPU - PgVector backend
docker build -t conduit-memoria:pgvector-gpu \
  --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
  --build-arg BACKEND=pgvector \
  --build-arg DEVICE=cuda services/memoria

# GPU - Qdrant backend
docker build -t conduit-memoria:qdrant-gpu \
  --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
  --build-arg BACKEND=qdrant \
  --build-arg DEVICE=cuda services/memoria
```

## Storage

### Builtin Backend

JSON file storage with in-memory BM25 search:
- No external dependencies
- Suitable for small deployments (<10K engrams)
- Fast startup and simple deployment
- Persistent storage in single JSON file

### PgVector Backend

PostgreSQL with vector embeddings:
- Persistent database storage
- Vector embeddings for semantic search
- Scalable to millions of engrams
- Requires PostgreSQL with pgvector extension
- Embedding model downloaded on first use

**Deployment**:
```bash
docker compose --profile memoria-pgvector up
```

### Qdrant Backend

Qdrant vector database:
- Purpose-built vector database
- High performance semantic search
- Horizontal scaling capabilities
- Self-contained, no extension dependencies
- Embedding model downloaded on first use

**Deployment**:
```bash
docker compose --profile memoria-qdrant up
```

## Tests

The tests use mock encoders and storage, so they download no models:

```bash
python -m venv .venv && .venv/bin/pip install -r requirements-dev.txt
.venv/bin/python -m pytest
```

## Conduit Integration

Memoria integrates with Conduit through the HTTP memory provider:

```json
{
  "name": "memoria",
  "kind": "memory",
  "type": "http",
  "config": {
    "base_url": "http://memoria:8080",
    "api_key": "your-api-key"
  }
}
```

When linked, Memoria syncs with Conduit's speaker and conversation rosters, and Conduit can use Memoria as its memory backend in pipelines.

## MCP Integration

Memoria can be used as an MCP server:

```json
{
  "name": "memoria",
  "transport": {
    "type": "stdio",
    "command": ["python", "-m", "mcp_server"],
    "env": {
      "MEMORIA_BASE_URL": "http://memoria:8080",
      "MEMORIA_API_KEY": "your-api-key"
    }
  }
}
```

## Security

- Bearer token authentication on all routes except `/health`
- Tokens never logged and stored with restricted permissions
- Local API key generation when linking
- Input validation and sanitization
- Rate limiting on search endpoints

## Future Enhancements

- Memory expiration and TTL
- Memory importance scoring
- Cross-session learning
- Memory clustering
- Export/import capabilities
- Analytics and insights