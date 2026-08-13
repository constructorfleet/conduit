# Build context is the repository root, not services/instrumenta: the image
# installs packages/conduit-link, a workspace-local package the service imports
# at runtime and that lives outside the service directory.
#
#   docker build -t conduit-instrumenta -f services/instrumenta/Dockerfile .
FROM python:3.13-slim AS base

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY services/instrumenta/requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

# Workspace-local package the service imports at runtime.
COPY packages/conduit-link ./conduit-link
RUN pip install --no-cache-dir ./conduit-link

COPY services/instrumenta/app.py services/instrumenta/backend.py services/instrumenta/secret_box.py services/instrumenta/builtins.py services/instrumenta/mcp_app.py services/instrumenta/aggregator.py services/instrumenta/servers_router.py services/instrumenta/metrics.py services/instrumenta/__init__.py ./instrumenta/
COPY services/instrumenta/static ./instrumenta/static

RUN mkdir -p /data
ENV INSTRUMENTA_DATA_DIR=/data
ENV INSTRUMENTA_BACKEND=sqlite
ENV PYTHONPATH=/app

EXPOSE 8085 9090

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8085/health || exit 1

CMD ["python", "-m", "uvicorn", "instrumenta.app:create_app", "--factory", "--host", "0.0.0.0", "--port", "8085"]
