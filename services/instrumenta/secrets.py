"""Fernet-backed secret storage.

Upstream MCP server credentials (PATs, API keys) are stored encrypted at
rest. The key comes from `INSTRUMENTA_SECRET_KEY`; if any encrypted row
exists in the backend and no key is set, the app refuses to start —
misconfiguration surfaces at boot rather than at first-decrypt-failure.

Fernet's built-in token versioning gives us a straightforward path to key
rotation later (multi-key `MultiFernet`), which is why it beats hazmat AES-GCM
here for a workload that is not remotely performance-sensitive.
"""

from __future__ import annotations


class SecretKeyMissingError(RuntimeError):
    """Raised when encrypted secrets exist but no key is configured."""


class SecretBox:
    """Encrypt/decrypt secrets with a key from `INSTRUMENTA_SECRET_KEY`.

    Constructed with an optional key. `encrypt`/`decrypt` require a key;
    `can_decrypt()` reports whether one is present so the boot check can
    fail loud when secrets exist without a key.
    """

    def __init__(self, key: str | None):
        self._key = key
        self._fernet = None
        if key:
            try:
                from cryptography.fernet import Fernet

                self._fernet = Fernet(key.encode())
            except Exception as exc:
                raise SecretKeyMissingError(
                    f"INSTRUMENTA_SECRET_KEY is not a valid Fernet key: {exc}"
                ) from exc

    def can_decrypt(self) -> bool:
        return self._fernet is not None

    def encrypt(self, plaintext: str) -> bytes:
        if self._fernet is None:
            raise SecretKeyMissingError(
                "cannot encrypt: INSTRUMENTA_SECRET_KEY not set"
            )
        return self._fernet.encrypt(plaintext.encode())

    def decrypt(self, ciphertext: bytes) -> str:
        if self._fernet is None:
            raise SecretKeyMissingError(
                "cannot decrypt: INSTRUMENTA_SECRET_KEY not set"
            )
        return self._fernet.decrypt(ciphertext).decode()
