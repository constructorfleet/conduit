-- One row per linked Conduit Vox peer.
--
-- The peer chooses its own id and Conduit keys the row on it, so a peer that
-- reboots and re-links replaces its own row rather than accumulating stale
-- ones. The raw sync token is never stored: the row carries its SHA-256, and
-- the token itself is handed back to the caller of POST /v1/vox/links once.
--
-- The document stays as jsonb for the same reason the other definitions do:
-- the API owns a typed document and callers round-trip it whole.
CREATE TABLE IF NOT EXISTS vox_links (
    peer_id    TEXT PRIMARY KEY,
    link       JSONB       NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
