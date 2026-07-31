-- Pipeline definitions, one row each.
--
-- The graph is stored as jsonb rather than shredded into tables: it is a
-- document the editor round-trips whole, and nothing here queries inside it.
-- Shredding would buy nothing and cost a migration every time a node kind is
-- added.
CREATE TABLE IF NOT EXISTS pipelines (
    name       TEXT PRIMARY KEY,
    graph      JSONB       NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
