"""Persistence for the linked-service state, generic over per-service extras.

`LinkStore[E]` composes the base `LinkState` (spec-0005 fields) with a
per-service extension dataclass `E` (e.g. `VoxLinkExtension`). The extension
is serialised into the same `link.json` under a reserved `extension` key so
the file remains one atomic write governed by the 0600 permission invariant.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Generic, TypeVar

from .models import LinkState

E = TypeVar("E")


@dataclass(frozen=True)
class LinkRecord(Generic[E]):
    state: LinkState
    extension: E


class LinkStore(Generic[E]):
    """The persisted relationship with one Conduit instance.

    Enforces spec-0005 §Rules that must hold bullet 1 by refusing to read the
    file if permissions are looser than 0600.
    """

    FILENAME = "link.json"

    def __init__(
        self,
        directory: Path,
        *,
        extension_from_dict: Callable[[dict[str, object]], E],
        extension_to_dict: Callable[[E], dict[str, object]],
    ) -> None:
        raise NotImplementedError

    def load(self) -> LinkRecord[E] | None:
        """Return the persisted record, or None if unlinked.

        Raises `LinkStoreSecurityError` if the file's permissions are looser
        than 0600.
        """
        raise NotImplementedError

    def save(self, state: LinkState, extension: E) -> LinkRecord[E]:
        """Atomically write a new record (0600, temp-file + rename)."""
        raise NotImplementedError

    def remove(self) -> None:
        """Delete the persisted record if present. No-op if already unlinked."""
        raise NotImplementedError
