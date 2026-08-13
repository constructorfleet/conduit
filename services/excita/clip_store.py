"""On-disk storage for raw audio clip bytes.

Content-addressed by sha256 so re-uploading the same audio is a no-op at the
filesystem layer and the DB's `UNIQUE (phrase_id, sha256)` catches it at the
row layer. Kept trivial on purpose — the interesting invariants live in
`backend.py`.
"""

from __future__ import annotations

import hashlib
from pathlib import Path


_EXT_BY_MIME = {
    "audio/wav": ".wav",
    "audio/x-wav": ".wav",
    "audio/wave": ".wav",
    "audio/ogg": ".ogg",
    "audio/webm": ".webm",
}


class UnsupportedMimeError(ValueError):
    pass


class ClipStore:
    def __init__(self, root: Path) -> None:
        self._root = root
        self._root.mkdir(parents=True, exist_ok=True)

    def store(self, data: bytes, mime_type: str) -> tuple[str, Path]:
        ext = _EXT_BY_MIME.get(mime_type)
        if ext is None:
            raise UnsupportedMimeError(f"unsupported mime type: {mime_type}")
        digest = hashlib.sha256(data).hexdigest()
        path = self._root / f"{digest}{ext}"
        if not path.exists():
            path.write_bytes(data)
        return digest, path

    def read(self, path: str | Path) -> bytes:
        return Path(path).read_bytes()
