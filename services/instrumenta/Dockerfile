FROM python:3.13-slim AS base

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

COPY app.py backend.py secret_box.py builtins.py mcp_app.py __init__.py ./instrumenta/
COPY static ./instrumenta/static

RUN mkdir -p /data
ENV INSTRUMENTA_DATA_DIR=/data
ENV INSTRUMENTA_BACKEND=sqlite
ENV PYTHONPATH=/app

EXPOSE 8085

HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8085/health || exit 1

CMD ["python", "-m", "uvicorn", "instrumenta.app:create_app", "--factory", "--host", "0.0.0.0", "--port", "8085"]
