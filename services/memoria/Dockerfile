FROM python:3.13-slim

# Install system dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy requirements first for better caching
COPY requirements*.txt ./

# Install Python dependencies
RUN pip install --no-cache-dir -r requirements.txt

# If using pgvector backend, install additional dependencies
ARG BACKEND=builtin
RUN if [ "$BACKEND" = "pgvector" ]; then \
    pip install --no-cache-dir -r requirements-pgvector.txt; \
    fi

# If using qdrant backend, install additional dependencies
RUN if [ "$BACKEND" = "qdrant" ]; then \
    pip install --no-cache-dir -r requirements-qdrant.txt; \
    fi

# If using GPU, install torch with CUDA support
ARG DEVICE=cpu
RUN if [ "$DEVICE" = "cuda" ]; then \
    pip install --no-cache-dir torch torchvision torchaudio --index-url https://download.pytorch.org/whl/cu121; \
    else \
    pip install --no-cache-dir torch torchvision torchaudio --index-url https://download.pytorch.org/whl/cpu; \
    fi

# Copy application code
COPY app.py builtin_backend.py pgvector_backend.py qdrant_backend.py mcp_server.py metrics.py ./

# Create data directory
RUN mkdir -p /data /cache

# Set environment variables
ENV MEMORIA_DATA_DIR=/data
ENV MEMORIA_CACHE_DIR=/cache
ENV MEMORIA_BACKEND=${BACKEND}
ENV MEMORIA_MCP_ENABLED=false

# Expose ports
# 8080: HTTP API and UI
# 8081: MCP server (HTTP/SSE)
EXPOSE 8080 8081 9090

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Run the application
CMD ["python", "-m", "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080"]
