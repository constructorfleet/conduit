"""Qdrant storage backend using vector embeddings."""

import asyncio
import logging
import os
from typing import Any

import numpy as np

LOG = logging.getLogger("memoria.qdrant")


class QdrantBackend:
    """Qdrant vector database for semantic search."""

    def __init__(self, qdrant_url: str, collection_name: str = "memoria"):
        self.qdrant_url = qdrant_url
        self.collection_name = collection_name
        self.embedder: Embedder | None = None
        self.client = None
        self._initialized = False

    async def _initialize(self) -> None:
        """Initialize Qdrant client and collection."""
        if self._initialized:
            return

        try:
            from qdrant_client import QdrantClient
            from qdrant_client.models import Distance, VectorParams, PointStruct, Filter, FieldCondition, MatchValue

            self.client = QdrantClient(url=self.qdrant_url, timeout=30.0)

            # Check if collection exists, create if not
            collections = self.client.get_collections().collections
            collection_names = [c.name for c in collections]

            if self.collection_name not in collection_names:
                self.client.create_collection(
                    collection_name=self.collection_name,
                    vectors_config=VectorParams(
                        size=384,  # all-MiniLM-L6-v2 dimension
                        distance=Distance.COSINE,
                    ),
                )
                LOG.info(f"Created Qdrant collection: {self.collection_name}")

            # Initialize embedder
            self.embedder = Embedder()
            await self.embedder.initialize()

            self._initialized = True
            LOG.info("Initialized qdrant backend")

        except Exception as e:
            LOG.error(f"Failed to initialize qdrant backend: {e}")
            raise

    async def store(self, engram: Any) -> Any:
        """Store a new engram."""
        await self._initialize()

        # Generate embedding
        embedding = await self.embedder.embed(engram.content)

        from qdrant_client.models import PointStruct

        point = PointStruct(
            id=engram.id,
            vector=embedding.tolist(),
            payload={
                "content": engram.content,
                "speaker_id": engram.speaker_id,
                "conversation_id": engram.conversation_id,
                "scope": engram.scope,
                "metadata": engram.metadata or {},
                "created_at": engram.created_at,
                "updated_at": engram.updated_at,
            },
        )

        self.client.upsert(collection_name=self.collection_name, points=[point])
        return engram

    async def get(self, engram_id: str) -> Any | None:
        """Retrieve an engram by ID."""
        await self._initialize()

        try:
            result = self.client.retrieve(
                collection_name=self.collection_name,
                ids=[engram_id],
                with_payload=True,
            )

            if not result:
                return None

            point = result[0]
            return self._point_to_engram(point)

        except Exception as e:
            LOG.error(f"Failed to get engram {engram_id}: {e}")
            return None

    async def update(self, engram_id: str, updates: dict[str, Any]) -> Any | None:
        """Update an engram."""
        await self._initialize()

        # Get existing point
        existing = await self.get(engram_id)
        if not existing:
            return None

        # Build updated payload
        payload = {
            "content": existing.content,
            "speaker_id": existing.speaker_id,
            "conversation_id": existing.conversation_id,
            "scope": existing.scope,
            "metadata": existing.metadata or {},
            "created_at": existing.created_at,
            "updated_at": datetime.now(timezone.utc).isoformat(),
        }

        # Apply updates
        if "content" in updates:
            payload["content"] = updates["content"]
        if "metadata" in updates:
            payload["metadata"] = updates["metadata"] or {}

        # Generate new embedding if content changed
        if "content" in updates:
            embedding = await self.embedder.embed(updates["content"])
            from qdrant_client.models import PointStruct

            point = PointStruct(
                id=engram_id,
                vector=embedding.tolist(),
                payload=payload,
            )
            self.client.upsert(collection_name=self.collection_name, points=[point])
        else:
            self.client.set_payload(
                collection_name=self.collection_name,
                payload=payload,
                points=[engram_id],
            )

        return await self.get(engram_id)

    async def delete(self, engram_id: str) -> bool:
        """Delete an engram."""
        await self._initialize()

        try:
            self.client.delete(collection_name=self.collection_name, points_selector=[engram_id])
            return True
        except Exception as e:
            LOG.error(f"Failed to delete engram {engram_id}: {e}")
            return False

    async def list(
        self,
        scope: str | None = None,
        speaker_id: str | None = None,
        conversation_id: str | None = None,
        limit: int = 100,
    ) -> list[Any]:
        """List engrams with optional filters."""
        await self._initialize()

        from qdrant_client.models import Filter, FieldCondition, MatchValue

        # Build filter
        conditions = []
        if scope:
            conditions.append(FieldCondition(key="scope", match=MatchValue(value=scope)))
        if speaker_id:
            conditions.append(FieldCondition(key="speaker_id", match=MatchValue(value=speaker_id)))
        if conversation_id:
            conditions.append(FieldCondition(key="conversation_id", match=MatchValue(value=conversation_id)))

        query_filter = Filter(must=conditions) if conditions else None

        try:
            results = self.client.scroll(
                collection_name=self.collection_name,
                scroll_filter=query_filter,
                limit=limit,
                with_payload=True,
            )

            points = results[0] if results else []
            return [self._point_to_engram(point) for point in points]

        except Exception as e:
            LOG.error(f"Failed to list engrams: {e}")
            return []

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

        from qdrant_client.models import Filter, FieldCondition, MatchValue

        # Build filter
        conditions = []
        if scope:
            conditions.append(FieldCondition(key="scope", match=MatchValue(value=scope)))
        if speaker_id:
            conditions.append(FieldCondition(key="speaker_id", match=MatchValue(value=speaker_id)))
        if conversation_id:
            conditions.append(FieldCondition(key="conversation_id", match=MatchValue(value=conversation_id)))

        query_filter = Filter(must=conditions) if conditions else None

        try:
            results = self.client.search(
                collection_name=self.collection_name,
                query_vector=query_embedding.tolist(),
                query_filter=query_filter,
                limit=limit,
                with_payload=True,
            )

            return [
                (self._point_to_engram(result), max(0.0, result.score)) for result in results
            ]

        except Exception as e:
            LOG.error(f"Failed to search engrams: {e}")
            return []

    def _point_to_engram(self, point) -> Any:
        """Convert Qdrant point to Engram object."""
        from app import Engram
        payload = point.payload

        return Engram(
            id=str(point.id),
            content=payload.get("content", ""),
            speaker_id=payload.get("speaker_id"),
            conversation_id=payload.get("conversation_id"),
            scope=payload.get("scope", "global"),
            metadata=payload.get("metadata"),
            created_at=payload.get("created_at", ""),
            updated_at=payload.get("updated_at", ""),
        )

    async def health(self) -> dict[str, Any]:
        """Return backend health status."""
        await self._initialize()

        try:
            collection_info = self.client.get_collection(self.collection_name)
            count = collection_info.points_count

            return {
                "backend": "qdrant",
                "status": "ok",
                "engram_count": count,
                "collection": self.collection_name,
                "embedding_model": self.embedder.model_name if self.embedder else "not initialized",
            }
        except Exception as e:
            LOG.error(f"Health check failed: {e}")
            return {
                "backend": "qdrant",
                "status": "error",
                "engram_count": 0,
                "error": str(e),
            }

    async def cleanup(self) -> None:
        """Cleanup resources."""
        if self.client:
            self.client.close()
            self.client = None


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


from datetime import datetime, timezone