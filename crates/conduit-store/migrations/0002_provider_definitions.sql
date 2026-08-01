-- Provider definitions, one row each.
--
-- The definition stays as jsonb because the API owns a closed typed document
-- shape and callers round-trip it whole. Querying provider ids is enough for
-- the store contract; runtime registration decodes the typed document.
CREATE TABLE IF NOT EXISTS provider_definitions (
    id         TEXT PRIMARY KEY,
    definition JSONB       NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
