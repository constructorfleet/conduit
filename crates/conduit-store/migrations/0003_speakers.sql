-- The people a deployment has enrolled, one row each.
--
-- The identification service holds the voice prints under an opaque label;
-- this is the only place that label is tied to a person's name, which is why
-- it is Conduit's row and not the service's.
--
-- The entry stays as jsonb for the same reason the other definitions do: the
-- API owns a typed document and callers round-trip it whole. The id is the
-- key because it is what a turn reports and what the service was told.
CREATE TABLE IF NOT EXISTS speakers (
    id         TEXT PRIMARY KEY,
    speaker    JSONB       NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
