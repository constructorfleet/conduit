"""Exceptions raised by the shared link module."""

from __future__ import annotations


class LinkStoreSecurityError(RuntimeError):
    """Raised when `link.json` is readable by anyone except the service user.

    Upholds spec 0005 §Rules that must hold bullet 1 — `sync_token` (and, once
    §Handshake is amended, `peer_token`) is stored on disk only in a mode-0600
    file.
    """


class LinkConflict(RuntimeError):
    """Raised when a link already exists and `force=True` was not supplied."""


class LinkNotFound(RuntimeError):
    """Raised when an operation expected an existing link but found none."""
