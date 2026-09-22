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

# The whole package, not a list of modules: the list had gone stale and the
# image shipped without `audit`, `items_router`, `path_probe` and
# `stdio_supervisor`, so importing `instrumenta.app` failed at boot. A module
# added later cannot be forgotten this way. Developer virtualenvs and caches
# are excluded by .dockerignore.
COPY services/instrumenta ./instrumenta

RUN mkdir -p /data
ENV INSTRUMENTA_DATA_DIR=/data
ENV INSTRUMENTA_BACKEND=sqlite
ENV PYTHONPATH=/app

EXPOSE 8085 9090

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8085/health || exit 1

CMD ["python", "-m", "uvicorn", "instrumenta.app:create_app", "--factory", "--host", "0.0.0.0", "--port", "8085"]
