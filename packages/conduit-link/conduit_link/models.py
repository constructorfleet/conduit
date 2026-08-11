"""Wire-facing dataclasses shared by every Conduit-linked service.

These mirror the Rust types in `crates/conduit-link` (spec 0005 §Identity,
§Panel manifest, §Reachability). Base state carries only the fields spec 0005
defines as generic across services; per-service extras ride in a parameterised
extension dataclass — composition, not subclassing.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum


class LinkedServiceKind(str, Enum):
    VOX = "vox"
    MEMORIA = "memoria"
    INSTRUMENTA = "instrumenta"
    EXCITA = "excita"
    DICTA = "dicta"
    FORMA = "forma"
    GENERIC = "generic"


class Reachability(str, Enum):
    UNKNOWN = "unknown"
    REACHABLE = "reachable"
    UNREACHABLE = "unreachable"


@dataclass(frozen=True)
class LinkedServicePanel:
    """Fallback panel manifest (spec 0005 §Panel manifest)."""

    title: str
    path: str
    icon: str | None = None


@dataclass(frozen=True)
class LinkStatus:
    peer_id: str
    peer_name: str
    reachability: Reachability
    last_seen_at: str | None


@dataclass(frozen=True)
class LinkState:
    """The generic 0005 fields — every linked service persists exactly these."""

    conduit_url: str
    peer_id: str
    peer_name: str
    sync_token: str
    panel: LinkedServicePanel
    linked_at: str
