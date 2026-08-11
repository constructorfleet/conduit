"""Construction-time configuration for the shared link module.

Each service reads env/config in its own bootstrap and passes a `LinkConfig`
in. The module itself performs no environment lookups — explicitness over
magic (per AGENTS.md).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from .models import LinkedServicePanel, LinkedServiceKind


@dataclass(frozen=True)
class LinkConfig:
    service_kind: LinkedServiceKind
    peer_name: str
    peer_base_url: str
    panel: LinkedServicePanel
    storage_dir: Path
