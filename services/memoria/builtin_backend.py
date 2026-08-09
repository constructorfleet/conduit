"""Builtin storage backend using JSON files and BM25 search."""

import json
import logging
import math
import re
import threading
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import numpy as np

LOG = logging.getLogger("memoria.builtin")


class BuiltinBackend:
    """JSON file storage with BM25 keyword search."""

    def __init__(self, storage_path: Path):
        self.storage_path = storage_path
        self.lock = threading.RLock()
        self._load_data()

    def _load_data(self) -> None:
        """Load engrams from storage file."""
        self.engrams: dict[str, dict[str, Any]] = {}
        self.index: dict[str, dict[str, float]] = {}  # term -> doc_id -> score

        if self.storage_path.exists():
            try:
                data = json.loads(self.storage_path.read_text())
                self.engrams = data.get("engrams", {})
                self._rebuild_index()
            except Exception as e:
                LOG.warning(f"Failed to load storage: {e}")
                self.engrams = {}

    def _save_data(self) -> None:
        """Save engrams to storage file."""
        self.storage_path.parent.mkdir(parents=True, exist_ok=True)
        data = {"engrams": self.engrams}
        self.storage_path.write_text(json.dumps(data, indent=2))

    def _rebuild_index(self) -> None:
        """Rebuild the BM25 index from all engrams."""
        self.index = {}
        if not self.engrams:
            return

        # Tokenize all documents
        documents: list[str] = []
        doc_ids: list[str] = []
        for doc_id, engram_data in self.engrams.items():
            content = engram_data.get("content", "")
            documents.append(content)
            doc_ids.append(doc_id)

        # Calculate document frequencies
        doc_freqs: Counter = Counter()
        doc_lengths: list[int] = []
        avg_doc_length = 0

        for content in documents:
            tokens = self._tokenize(content)
            doc_lengths.append(len(tokens))
            unique_tokens = set(tokens)
            doc_freqs.update(unique_tokens)

        if doc_lengths:
            avg_doc_length = sum(doc_lengths) / len(doc_lengths)

        # BM25 parameters
        k1 = 1.2
        b = 0.75
        N = len(documents)

        # Calculate BM25 scores for each term in each document
        for i, (content, doc_id) in enumerate(zip(documents, doc_ids)):
            tokens = self._tokenize(content)
            doc_length = len(tokens)

            for token in set(tokens):
                # Calculate IDF
                df = doc_freqs.get(token, 0)
                idf = math.log((N - df + 0.5) / (df + 0.5) + 1.0)

                # Calculate TF component
                tf = tokens.count(token)
                numerator = tf * (k1 + 1)
                denominator = tf + k1 * (1 - b + b * (doc_length / avg_doc_length))
                bm25 = idf * (numerator / denominator)

                if token not in self.index:
                    self.index[token] = {}
                self.index[token][doc_id] = bm25

    def _tokenize(self, text: str) -> list[str]:
        """Tokenize text into terms."""
        # Lowercase and split on non-alphanumeric
        tokens = re.findall(r"\w+", text.lower())
        # Remove single-character tokens
        return [t for t in tokens if len(t) > 1]

    async def store(self, engram: Any) -> Any:
        """Store a new engram."""
        with self.lock:
            engram_dict = engram.model_dump()
            engram_id = engram_dict["id"]
            self.engrams[engram_id] = engram_dict
            self._rebuild_index()
            self._save_data()
            return engram

    async def get(self, engram_id: str) -> Any | None:
        """Retrieve an engram by ID."""
        with self.lock:
            engram_data = self.engrams.get(engram_id)
            if engram_data is None:
                return None
            # Dynamically get the Engram class from the main app
            from app import Engram
            return Engram(**engram_data)

    async def update(self, engram_id: str, updates: dict[str, Any]) -> Any | None:
        """Update an engram."""
        with self.lock:
            if engram_id not in self.engrams:
                return None

            engram_data = self.engrams[engram_id]
            engram_data.update(updates)
            engram_data["updated_at"] = datetime.now(timezone.utc).isoformat()

            self._rebuild_index()
            self._save_data()

            from app import Engram
            return Engram(**engram_data)

    async def delete(self, engram_id: str) -> bool:
        """Delete an engram."""
        with self.lock:
            if engram_id not in self.engrams:
                return False

            del self.engrams[engram_id]
            self._rebuild_index()
            self._save_data()
            return True

    async def list(
        self,
        scope: str | None = None,
        speaker_id: str | None = None,
        conversation_id: str | None = None,
        limit: int = 100,
    ) -> list[Any]:
        """List engrams with optional filters."""
        with self.lock:
            results = []
            from app import Engram

            for engram_data in self.engrams.values():
                # Apply filters
                if scope and engram_data.get("scope") != scope:
                    continue
                if speaker_id and engram_data.get("speaker_id") != speaker_id:
                    continue
                if conversation_id and engram_data.get("conversation_id") != conversation_id:
                    continue

                results.append(Engram(**engram_data))
                if len(results) >= limit:
                    break

            return results

    async def search(
        self,
        query: str,
        limit: int = 10,
        scope: str | None = None,
        speaker_id: str | None = None,
        conversation_id: str | None = None,
    ) -> list[tuple[Any, float]]:
        """Search engrams using BM25 scoring."""
        with self.lock:
            # Tokenize query
            query_tokens = self._tokenize(query)

            if not query_tokens:
                return []

            # Calculate scores for each document
            doc_scores: dict[str, float] = {}

            for token in query_tokens:
                if token not in self.index:
                    continue

                for doc_id, bm25 in self.index[token].items():
                    if doc_id not in doc_scores:
                        doc_scores[doc_id] = 0
                    doc_scores[doc_id] += bm25

            # Filter and normalize results
            results = []
            max_score = 0

            for doc_id, score in doc_scores.items():
                engram_data = self.engrams.get(doc_id)
                if not engram_data:
                    continue

                # Apply filters
                if scope and engram_data.get("scope") != scope:
                    continue
                if speaker_id and engram_data.get("speaker_id") != speaker_id:
                    continue
                if conversation_id and engram_data.get("conversation_id") != conversation_id:
                    continue

                results.append((doc_id, score))
                if score > max_score:
                    max_score = score

            # Normalize scores to 0-1 range
            if max_score > 0:
                results = [(doc_id, score / max_score) for doc_id, score in results]

            # Sort by score (descending) and limit
            results.sort(key=lambda x: x[1], reverse=True)
            results = results[:limit]

            # Return engram objects with scores
            from app import Engram
            return [
                (Engram(**self.engrams[doc_id]), score) for doc_id, score in results
            ]

    async def health(self) -> dict[str, Any]:
        """Return backend health status."""
        with self.lock:
            return {
                "backend": "builtin",
                "status": "ok",
                "engram_count": len(self.engrams),
                "index_size": len(self.index),
            }

    async def cleanup(self) -> None:
        """Cleanup resources."""
        with self.lock:
            self._save_data()