"""Shared Python implementation of the Conduit link protocol (spec 0005).

Re-exports the stable public API. Consumers should import from
`conduit_link`, not from submodules.
"""

from __future__ import annotations

from .client import (
    ConduitLinkClient,
    HttpConduitLinkClient,
    InMemoryConduitLinkClient,
)
from .config import LinkConfig
from .errors import LinkConflict, LinkNotFound, LinkStoreSecurityError
from .models import (
    LinkedServiceKind,
    LinkedServicePanel,
    LinkState,
    LinkStatus,
    Reachability,
)
from .router import LinkRequest, make_link_router
from .store import LinkRecord, LinkStore

__all__ = [
    "ConduitLinkClient",
    "HttpConduitLinkClient",
    "InMemoryConduitLinkClient",
    "LinkConfig",
    "LinkConflict",
    "LinkNotFound",
    "LinkRecord",
    "LinkRequest",
    "LinkState",
    "LinkStatus",
    "LinkStore",
    "LinkStoreSecurityError",
    "LinkedServiceKind",
    "LinkedServicePanel",
    "Reachability",
    "make_link_router",
]
