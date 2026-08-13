# Build context is the repository root, not services/memoria: the image
# installs packages/conduit-link, a workspace-local package the service imports
# at runtime and that lives outside the service directory.
#
#   docker build -t conduit-memoria:builtin -f services/memoria/Dockerfile .
FROM python:3.13-slim

RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY services/memoria/requirements*.txt ./

RUN pip install --no-cache-dir -r requirements.txt

ARG BACKEND=builtin
RUN if [ "$BACKEND" = "pgvector" ]; then \
    pip install --no-cache-dir -r requirements-pgvector.txt; \
    fi

RUN if [ "$BACKEND" = "qdrant" ]; then \
    pip install --no-cache-dir -r requirements-qdrant.txt; \
    fi

ARG DEVICE=cpu
RUN if [ "$DEVICE" = "cuda" ]; then \
    pip install --no-cache-dir torch torchvision torchaudio --index-url https://download.pytorch.org/whl/cu121; \
    else \
    pip install --no-cache-dir torch torchvision torchaudio --index-url https://download.pytorch.org/whl/cpu; \
    fi

# Workspace-local package the service imports at runtime. Installed after the
# heavy torch layer so an edit to it does not invalidate that layer.
COPY packages/conduit-link ./conduit-link
RUN pip install --no-cache-dir ./conduit-link

COPY services/memoria/app.py services/memoria/builtin_backend.py services/memoria/pgvector_backend.py services/memoria/qdrant_backend.py services/memoria/mcp_server.py services/memoria/metrics.py ./

RUN mkdir -p /data /cache

ENV MEMORIA_DATA_DIR=/data
ENV MEMORIA_CACHE_DIR=/cache
ENV MEMORIA_BACKEND=${BACKEND}
ENV MEMORIA_MCP_ENABLED=false

# Expose ports
# 8080: HTTP API and UI
# 8081: MCP server (HTTP/SSE)
EXPOSE 8080 8081 9090

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

CMD ["python", "-m", "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080"]
