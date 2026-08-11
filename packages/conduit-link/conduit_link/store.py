"""Persistence for the linked-service state, generic over per-service extras.

`LinkStore[E]` composes the base `LinkState` (spec-0005 fields) with a
per-service extension dataclass `E` (e.g. `VoxLinkExtension`). The extension
is serialised into the same `link.json` under a reserved `extension` key so
the file remains one atomic write governed by the 0600 permission invariant.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Generic, TypeVar

from .errors import LinkStoreSecurityError
from .models import LinkedServicePanel, LinkState

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
        self.directory = directory
        self._path = directory / self.FILENAME
        self._ext_from = extension_from_dict
        self._ext_to = extension_to_dict

    def load(self) -> LinkRecord[E] | None:
        """Return the persisted record, or None if unlinked.

        Raises `LinkStoreSecurityError` if the file's permissions are looser
        than 0600.
        """
        if not self._path.exists():
            return None
        self._refuse_loose_permissions()
        data = json.loads(self._path.read_text())
        panel_data = data["panel"]
        state = LinkState(
            conduit_url=str(data["conduit_url"]),
            peer_id=str(data["peer_id"]),
            peer_name=str(data["peer_name"]),
            sync_token=str(data["sync_token"]),
            panel=LinkedServicePanel(
                title=str(panel_data["title"]),
                path=str(panel_data["path"]),
                icon=panel_data.get("icon"),
            ),
            linked_at=str(data["linked_at"]),
        )
        extension = self._ext_from(dict(data.get("extension", {})))
        return LinkRecord(state=state, extension=extension)

    def save(self, state: LinkState, extension: E) -> LinkRecord[E]:
        """Atomically write a new record (0600, temp-file + rename)."""
        payload: dict[str, object] = {
            "conduit_url": state.conduit_url,
            "peer_id": state.peer_id,
            "peer_name": state.peer_name,
            "sync_token": state.sync_token,
            "panel": {
                "title": state.panel.title,
                "path": state.panel.path,
                "icon": state.panel.icon,
            },
            "linked_at": state.linked_at,
            "extension": self._ext_to(extension),
        }
        self.directory.mkdir(parents=True, exist_ok=True)
        temporary = self._path.with_suffix(".json.tmp")
        flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
        with os.fdopen(os.open(temporary, flags, 0o600), "w") as file:
            json.dump(payload, file, indent=2)
        os.chmod(temporary, 0o600)
        temporary.replace(self._path)
        os.chmod(self._path, 0o600)
        return LinkRecord(state=state, extension=extension)

    def remove(self) -> None:
        """Delete the persisted record if present. No-op if already unlinked."""
        if self._path.exists():
            self._refuse_loose_permissions()
            self._path.unlink()

    def _refuse_loose_permissions(self) -> None:
        mode = self._path.stat().st_mode & 0o777
        if mode & 0o077:
            raise LinkStoreSecurityError(
                f"{self.FILENAME} permissions must be 0600; found {mode:03o}"
            )
