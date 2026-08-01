# conduit-store

Pipeline storage backends.

All backends implement `conduit_provider::storage::PipelineStore`. Backend
choice is configuration, not API behavior.

## Backends

| Backend | Type | Notes |
| --- | --- | --- |
| Memory | `MemoryStore` | In-process only; lost on restart. |
| File | `FileStore` | One pretty-printed JSON file per pipeline. |
| PostgreSQL | `PostgresStore` | Shared storage for multiple API replicas; enabled with the `postgres` feature. |

## Shared Contract

Every backend must:

- validate names on `get`, `put`, and `remove`
- return only usable names from `list`
- treat absence as `Ok(None)` or `Ok(false)`
- report unreadable or undecodable stored graphs as errors
- round-trip graphs losslessly
- report whether `put` replaced an existing pipeline

The conformance suite in `tests/conformance/mod.rs` is the executable version
of this contract.

## File Store

`FileStore` creates the target directory if needed and stores `<name>.json`
files. Writes go through `<name>.json.tmp` followed by rename, so a crash
during write leaves the previous file intact.

## PostgreSQL Store

`PostgresStore` connects with a small pool, applies embedded migrations, stores
graphs as `jsonb`, and uses one `INSERT ... ON CONFLICT DO UPDATE` statement for
writes. That avoids lost updates from read-then-write races between replicas.

PostgreSQL tests require `CONDUIT_TEST_POSTGRES_URL`.
