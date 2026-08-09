"""PgVector storage backend using PostgreSQL with vector embeddings."""

import asyncio
import logging
from typing import Any

import httpx
import numpy as np

LOG = logging.getLogger("memoria.pgvector")


class PgVectorBackend:
    """PostgreSQL with pgvector for semantic search."""

    def __init__(self, database_url: str):
        self.database_url = database_url
        self.embedder: Embedder | None = None
        self._initialized = False

    async def _initialize(self) -> None:
        """Initialize database connection and tables."""
        if self._initialized:
            return

        try:
            import asyncpg
            self.pool = await asyncpg.create_pool(self.database_url, min_size=2, max_size=10)

            async with self.pool.acquire() as conn:
                await conn.execute("""
                    CREATE TABLE IF NOT EXISTS engrams (
                        id UUID PRIMARY KEY,
                        content TEXT NOT NULL,
                        speaker_id UUID,
                        conversation_id UUID,
                        scope VARCHAR(20) NOT NULL,
                        metadata JSONB,
                        embedding vector(384),
                        created_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                        updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
                    )
                """)

                await conn.execute("""
                    CREATE INDEX IF NOT EXISTS idx_engrams_speaker
                    ON engrams(speaker_id) WHERE speaker_id IS NOT NULL
                """)

                await conn.execute("""
                    CREATE INDEX IF NOT EXISTS idx_engrams_conversation
                    ON engrams(conversation_id) WHERE conversation_id IS NOT NULL
                """)

                await conn.execute("""
                    CREATE INDEX IF NOT EXISTS idx_engrams_scope
                    ON engrams(scope)
                """)

                await conn.execute("""
                    CREATE INDEX IF NOT EXISTS idx_engrams_embedding
                    ON engrams USING ivfflat (embedding vector_cosine_ops)
                """)

            # Initialize embedder
            self.embedder = Embedder()
            await self.embedder.initialize()

            self._initialized = True
            LOG.info("Initialized pgvector backend")

        except Exception as e:
            LOG.error(f"Failed to initialize pgvector backend: {e}")
            raise

    async def store(self, engram: Any) -> Any:
        """Store a new engram."""
        await self._initialize()

        # Generate embedding
        embedding = await self.embedder.embed(engram.content)

        async with self.pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO engrams (id, content, speaker_id, conversation_id, scope, metadata, embedding)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                """,
                engram.id,
                engram.content,
                engram.speaker_id,
                engram.conversation_id,
                engram.scope,
                engram.metadata or {},
                embedding.tolist(),
            )

        return engram

    async def get(self, engram_id: str) -> Any | None:
        """Retrieve an engram by ID."""
        await self._initialize()

        async with self.pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT id, content, speaker_id, conversation_id, scope, metadata, created_at, updated_at
                FROM engrams WHERE id = $1
                """,
                engram_id,
            )

            if row is None:
                return None

            from app import Engram
            return Engram(
                id=str(row["id"]),
                content=row["content"],
                speaker_id=str(row["speaker_id"]) if row["speaker_id"] else None,
                conversation_id=str(row["conversation_id"]) if row["conversation_id"] else None,
                scope=row["scope"],
                metadata=dict(row["metadata"]) if row["metadata"] else None,
                created_at=row["created_at"].isoformat(),
                updated_at=row["updated_at"].isoformat(),
            )

    async def update(self, engram_id: str, updates: dict[str, Any]) -> Any | None:
        """Update an engram."""
        await self._initialize()

        # Build update query
        set_clauses = []
        params = []
        param_count = 1

        if "content" in updates:
            set_clauses.append(f"content = ${param_count}")
            params.append(updates["content"])
            param_count += 1

        if "metadata" in updates:
            set_clauses.append(f"metadata = ${param_count}")
            params.append(updates["metadata"] or {})
            param_count += 1

        if not set_clauses:
            return await self.get(engram_id)

        # Update embedding if content changed
        if "content" in updates:
            embedding = await self.embedder.embed(updates["content"])
            set_clauses.append(f"embedding = ${param_count}")
            params.append(embedding.tolist())
            param_count += 1

        set_clauses.append(f"updated_at = NOW()")
        params.append(engram_id)

        async with self.pool.acquire() as conn:
            await conn.execute(
                f"""
                UPDATE engrams SET {', '.join(set_clauses)}
                WHERE id = ${param_count}
                """,
                *params,
            )

        return await self.get(engram_id)

    async def delete(self, engram_id: str) -> bool:
        """Delete an engram."""
        await self._initialize()

        async with self.pool.acquire() as conn:
            result = await conn.execute(
                "DELETE FROM engrams WHERE id = $1",
                engram_id,
            )
            return result == "DELETE 1"

    async def list(
        self,
        scope: str | None = None,
        speaker_id: str | None = None,
        conversation_id: str | None = None,
        limit: int = 100,
    ) -> list[Any]:
        """List engrams with optional filters."""
        await self._initialize()

        # Build query
        conditions = []
        params = []
        param_count = 1

        if scope:
            conditions.append(f"scope = ${param_count}")
            params.append(scope)
            param_count += 1

        if speaker_id:
            conditions.append(f"speaker_id = ${param_count}")
            params.append(speaker_id)
            param_count += 1

        if conversation_id:
            conditions.append(f"conversation_id = ${param_count}")
            params.append(conversation_id)
            param_count += 1

        where_clause = f"WHERE {' AND '.join(conditions)}" if conditions else ""
        limit_clause = f"LIMIT {limit}"

        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                f"""
                SELECT id, content, speaker_id, conversation_id, scope, metadata, created_at, updated_at
                FROM engrams
                {where_clause}
                ORDER BY created_at DESC
                {limit_clause}
                """,
                *params,
            )

        from app import Engram
        return [
            Engram(
                id=str(row["id"]),
                content=row["content"],
                speaker_id=str(row["speaker_id"]) if row["speaker_id"] else None,
                conversation_id=str(row["conversation_id"]) if row["conversation_id"] else None,
                scope=row["scope"],
                metadata=dict(row["metadata"]) if row["metadata"] else None,
                created_at=row["created_at"].isoformat(),
                updated_at=row["updated_at"].isoformat(),
            )
            for row in rows
        ]

    async def search(
        self,
        query: str,
        limit: int = 10,
        scope: str | None = None,
        speaker_id: str | None = None,
        conversation_id: str | None = None,
    ) -> list[tuple[Any, float]]:
        """Search engrams using vector similarity."""
        await self._initialize()

        # Generate query embedding
        query_embedding = await self.embedder.embed(query)

        # Build query
        conditions = []
        params = []
        param_count = 1

        if scope:
            conditions.append(f"scope = ${param_count}")
            params.append(scope)
            param_count += 1

        if speaker_id:
            conditions.append(f"speaker_id = ${param_count}")
            params.append(speaker_id)
            param_count += 1

        if conversation_id:
            conditions.append(f"conversation_id = ${param_count}")
            params.append(conversation_id)
            param_count += 1

        where_clause = f"AND {' AND '.join(conditions)}" if conditions else ""
        params.extend([query_embedding.tolist(), limit])

        async with self.pool.acquire() as conn:
            rows = await conn.fetch(
                f"""
                SELECT
                    id, content, speaker_id, conversation_id, scope, metadata, created_at, updated_at,
                    1 - (embedding <=> $1) as similarity
                FROM engrams
                WHERE 1=1 {where_clause}
                ORDER BY embedding <=> $1
                LIMIT $2
                """,
                *params,
            )

        from app import Engram
        return [
            (
                Engram(
                    id=str(row["id"]),
                    content=row["content"],
                    speaker_id=str(row["speaker_id"]) if row["speaker_id"] else None,
                    conversation_id=str(row["conversation_id"]) if row["conversation_id"] else None,
                    scope=row["scope"],
                    metadata=dict(row["metadata"]) if row["metadata"] else None,
                    created_at=row["created_at"].isoformat(),
                    updated_at=row["updated_at"].isoformat(),
                ),
                max(0.0, float(row["similarity"])),  # Clamp negative similarities to 0
            )
            for row in rows
        ]

    async def health(self) -> dict[str, Any]:
        """Return backend health status."""
        await self._initialize()

        async with self.pool.acquire() as conn:
            count = await conn.fetchval("SELECT COUNT(*) FROM engrams")

        return {
            "backend": "pgvector",
            "status": "ok",
            "engram_count": count,
            "embedding_model": self.embedder.model_name if self.embedder else "not initialized",
        }

    async def cleanup(self) -> None:
        """Cleanup resources."""
        if hasattr(self, 'pool') and self.pool:
            await self.pool.close()


class Embedder:
    """Text embedder using sentence-transformers."""

    def __init__(self):
        self.model_name = "sentence-transformers/all-MiniLM-L6-v2"
        self.device = os.getenv("MEMORIA_EMBEDDING_DEVICE", "cpu")
        self.model = None
        self._initialized = False

    async def initialize(self) -> None:
        """Initialize the embedding model."""
        if self._initialized:
            return

        try:
            from sentence_transformers import SentenceTransformer

            self.model = SentenceTransformer(self.model_name, device=self.device)
            self._initialized = True
            LOG.info(f"Initialized embedding model: {self.model_name}")

        except Exception as e:
            LOG.error(f"Failed to initialize embedding model: {e}")
            raise

    async def embed(self, text: str) -> np.ndarray:
        """Generate embedding for text."""
        if not self._initialized:
            await self.initialize()

        # Run in thread pool to avoid blocking
        loop = asyncio.get_event_loop()
        embedding = await loop.run_in_executor(None, self.model.encode, text)
        return embedding

import os