# Memoria Design Document

## Overview

Memoria is Conduit's reference memory service that provides persistent storage and retrieval of conversation memories (engrams). It follows a hybrid architecture:

1. **HTTP Service**: Vox-style FastAPI service with UI, linking, and persistence
2. **MCP Server**: Optional MCP interface for tool-based memory access

Like Vox, it can run as a standalone service or be integrated with Conduit's UI.

## Architecture

### Dual Interface Pattern

**HTTP Interface** (Vox-style):
- FastAPI-based HTTP service
- Standalone container with optional Conduit linking
- Self-contained HTML UI for memory management
- Token-based authentication
- Background sync when linked
- Full CRUD operations on engrams

**MCP Interface** (Tool-based):
- Standard MCP server over stdio/HTTP/SSE
- Tools for memory operations
- Resources for memory access
- Seamless integration with MCP clients

### Service Components

```
┌─────────────────────────────────────────┐
│           Memoria Service              │
├─────────────────────────────────────────┤
│  HTTP API (FastAPI)          MCP Server  │
│  ├── POST /engrams           ├── Tools:  │
│  ├── GET /engrams/{id}      │   ├── store_engram
│  ├── PATCH /engrams/{id}    │   ├── search_engrams  
│  ├── DELETE /engrams/{id}   │   ├── get_engram
│  ├── GET /engrams           │   ├── list_engrams
│  ├── POST /engrams/search   │   ├── delete_engram
│  ├── GET /health            │   └── Resources:
│  ├── GET /link              │       └── engrams://
│  ├── POST /link             │
│  ├── DELETE /link           │
│  └── GET /ui (static)       │
└─────────────────────────────────────────┘
│         Storage Layer                    │
│  ├── Builtin (JSON + BM25)              │
│  └── PostgreSQL + pgvector (optional)   │
└─────────────────────────────────────────┘
```

### Core Concepts

**Engram**: A single memory record with:
- Unique identifier (UUID)
- Content (text)
- Speaker association (optional)
- Conversation association (optional) 
- Scope (global, conversation, speaker)
- Metadata (JSON)
- Timestamps (created_at, updated_at)
- Embedding (for vector search)

**Operations**:
- **Store**: Save a new engram
- **Lookup**: Retrieve engrams by ID, scope, speaker, conversation
- **Search**: Find relevant engrams (semantic or keyword)
- **Delete**: Remove engrams
- **Update**: Modify existing engram content

## API Design

### HTTP Endpoints

```
POST   /engrams                    - Store a new engram
GET    /engrams/{id}              - Retrieve specific engram  
PATCH  /engrams/{id}              - Update engram content
DELETE /engrams/{id}              - Delete an engram
GET    /engrams                   - List/search engrams
POST   /engrams/search/semantic   - Semantic search with embedding
POST   /engrams/search/keyword    - Keyword search
GET    /engrams/speakers/{speaker_id} - Get all engrams for a speaker
GET    /engrams/conversations/{conversation_id} - Get all engrams for a conversation
GET    /health                    - Service health check
GET    /link                      - Link status with Conduit
POST   /link                      - Link with Conduit
DELETE /link                      - Unlink from Conduit
```

### MCP Tools

```
Tools:
  - store_engram(content, speaker_id?, conversation_id?, scope?, metadata?)
  - search_engrams(query, limit?, scope?, speaker_id?, conversation_id?)
  - get_engram(id)
  - list_engrams(scope?, speaker_id?, conversation_id?, limit?)
  - delete_engram(id)
  - update_engram(id, content?, metadata?)

Resources:
  - engrams://                    - List all engrams
  - engrams://speakers/{id}       - Engrams for specific speaker
  - engrams://conversations/{id}  - Engrams for specific conversation
  - engrams://scope/{scope}       - Engrams by scope (global/conversation/speaker)
```

### Data Structures

```json
{
  "id": "uuid",
  "content": "text",
  "speaker_id": "uuid|null",
  "conversation_id": "uuid|null", 
  "scope": "global|conversation|speaker",
  "metadata": {},
  "created_at": "ISO8601",
  "updated_at": "ISO8601"
}
```

## Integration with Conduit

### Linking Flow
1. Conduit operator initiates link via UI
2. Memoria generates one-time operator token
3. Conduit exchanges operator token for scoped sync token
4. Both store peer identifiers
5. Background sync maintains consistency

### Sync Behavior
- Bidirectional sync of engram metadata
- Conduit remains source of truth for speaker/conversation mappings
- Memoria handles storage and search operations
- Periodic reconciliation with configurable intervals

### Provider Integration
- `memory_provider` capability in Conduit
- HTTP-based provider definition pointing to Memoria
- Supports both `Builtin` (keyword) and `PgVector` (semantic) backends

## UI Features

### Management Interface (`/ui`)
- **Dashboard**: Service health, engram counts by scope
- **Engram Browser**: Paginated list with filters (speaker, conversation, scope)
- **Engram Editor**: Add/edit engrams with metadata
- **Search Interface**: Both semantic and keyword search
- **Speaker View**: All engrams for a specific speaker
- **Conversation View**: All engrams for a conversation
- **Link Panel**: Conduit integration status and controls
- **Bulk Operations**: Batch delete, export

### Search Capabilities
- **Semantic Search**: Vector similarity with configurable threshold
- **Keyword Search**: BM25 scoring (mimics Conduit's builtin backend)
- **Faceted Filtering**: By speaker, conversation, scope, date range
- **Result Ranking**: Relevance score with configurable limits

## Storage Backends

### Builtin (Default)
- JSON file storage
- In-memory indexing
- BM25 keyword search
- No external dependencies
- Suitable for small deployments (<10K engrams)

### PostgreSQL + pgvector (Optional)
- Persistent database storage
- Vector embeddings for semantic search
- Scalable to millions of engrams
- Supports complex queries and analytics
- Requires PostgreSQL with pgvector extension

## Configuration

```bash
# Core Settings
MEMORIA_BACKEND=builtin|pgvector|qdrant
MEMORIA_API_KEY=optional_bearer_token
MEMORIA_BASE_URL=http://memoria:8080

# Storage
MEMORIA_DATA_DIR=/data
MEMORIA_DATABASE_URL=postgresql://...  # pgvector only
MEMORIA_QDRANT_URL=http://qdrant:6333  # qdrant only
MEMORIA_QDRANT_COLLECTION=memoria     # qdrant only

# Embedding (pgvector/qdrant only)
MEMORIA_EMBEDDING_MODEL=sentence-transformers/all-MiniLM-L6-v2
MEMORIA_EMBEDDING_DEVICE=cpu
MEMORIA_CACHE_DIR=/cache

# Conduit Link
MEMORIA_CONDUIT_URL=http://conduit:8080
MEMORIA_SYNC_INTERVAL_SECONDS=300
MEMORIA_SYNC_MAX_BACKOFF_SECONDS=900

# Search
MEMORIA_SEARCH_LIMIT=10
MEMORIA_SEARCH_TIMEOUT_MS=3000
MEMORIA_SIMILARITY_THRESHOLD=0.7
```

## Security

- Bearer token authentication on all routes except `/health`
- Tokens never logged and stored with restricted permissions
- Local API key generation when linking
- TLS support for production deployments
- Input validation and sanitization
- Rate limiting on search endpoints

## Testing Strategy

- Unit tests for core operations (store, lookup, search)
- Integration tests for API endpoints
- Concurrency tests for simultaneous operations
- Performance tests for large datasets
- Mock encoder for embedding tests (no model downloads)

## Deployment

### Docker Images
```bash
# CPU - Builtin backend
docker build -t conduit-memoria:builtin services/memoria

# CPU - PgVector backend  
docker build -t conduit-memoria:pgvector \
  --build-arg BACKEND=pgvector services/memoria

# GPU - PgVector backend
docker build -t conduit-memoria:pgvector-gpu \
  --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
  --build-arg BACKEND=pgvector \
  --build-arg DEVICE=cuda services/memoria
```

### Docker Compose
```yaml
services:
  memoria:
    image: conduit-memoria:builtin
    ports:
      - "8080:8080"
    volumes:
      - memoria_data:/data
    environment:
      - MEMORIA_API_KEY=${MEMORIA_API_KEY}
      - MEMORIA_BASE_URL=http://memoria:8080
      - MEMORIA_MCP_ENABLED=true
      - MEMORIA_MCP_TRANSPORT=stdio
    restart: unless-stopped

volumes:
  memoria_data:
```

### MCP Integration
```yaml
# Example MCP provider definition in Conduit
{
  "name": "memoria",
  "transport": {
    "type": "stdio",
    "command": ["memoria-mcp"],
    "env": {
      "MEMORIA_BASE_URL": "http://memoria:8080",
      "MEMORIA_API_KEY": "your-api-key"
    }
  }
}
```

## Migration and Backward Compatibility

- Versioned API with backward compatibility
- Data migration scripts between storage formats
- Graceful degradation when features unavailable
- Configuration validation at startup

## Future Enhancements

- **Memory Expiration**: TTL-based engram cleanup
- **Memory Importance**: User-weighted significance scoring
- **Cross-Session Learning**: Persistent memory patterns
- **Memory Clustering**: Automatic grouping of related engrams
- **Export/Import**: Backup and restore capabilities
- **Analytics**: Memory usage patterns and insights