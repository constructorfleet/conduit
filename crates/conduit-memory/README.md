# conduit-memory

Memory store implementations for Conduit.

This crate implements `conduit_provider::memory::Memory` twice, for two very
different deployments.

| Backend | Ranking | Needs | Feature |
| --- | --- | --- | --- |
| `Builtin` | BM25 over unigrams | nothing at all | default |
| `PgVector` | cosine distance over an embedding | PostgreSQL, ideally with `pgvector` | `postgres` |

Which one a deployment runs is configuration. What it is *not* is invisible: a
keyword store and a vector store retrieve genuinely different records for the
same question, and no shared contract hides that.

## Scoring

Both backends produce a `Match.score` in `0.0..=1.0`, which the trait documents
as "comparable only within one result set".

### BM25, and why there is no stopword list

`Builtin` ranks with Okapi BM25 over unigrams, `k1 = 1.2` and `b = 0.75`.
Tokenising lowercases and splits on anything that is not alphanumeric, then drops
single-character tokens.

There is no stemmer and no stopword list. Both are language assumptions, and
nothing in a `Record` says what language the transcript was in — an English
stopword list applied to a German transcript throws away content words. What
replaces a stopword list is the inverse document frequency term, which gives a
word appearing in every record almost no weight without anyone having to name
which words those are. Not stemming costs recall on inflections; guessing the
language wrong costs correctness.

BM25 is unbounded, so the result set is normalised by its own best score. That is
legitimate *precisely* because comparability is scoped to one result set: a
caller may not compare a score from one search against a score from another, so
normalising per search breaks no promise it was allowed to rely on. The
consequence is that the best match always scores exactly `1.0`.

A record no query term appears in scores zero and is dropped rather than returned
with `score: 0.0`. A caller asked for what matched.

Filtering by conversation, speaker, and scope happens **before** scoring. That is
not only about wasted work: a record excluded by a filter must not contribute to
the document frequencies the surviving records are scored from, or the scores of
a per-conversation search would depend on other conversations.

Ties break by recency, by supplying candidates newest-first to a stable sort, so
recency never enters the score itself.

Full rescoring per search is fine at this scale — the default capacity is 1000
records and a typical limit is 5. If it ever is not, the fix is an inverted
index, not different scoring.

### Cosine distance

`PgVector` ranks by cosine, `1 - (embedding <=> query)`. Embedding models are
trained for cosine, and it ignores vector magnitude, which for an embedding is an
artefact of text length rather than of meaning.

pgvector's `<=>` is cosine *distance* in `0..2`, so similarity is `1 - distance`
and the negative half is clamped to `0.0`: a record pointing away from the query
is irrelevant, not negatively relevant. Rescaling instead would give an unrelated
record a middling score.

Ordering is by the raw `<=>` operator, not by the computed similarity, because
the index is only usable in that form. `ORDER BY 1 - (a <=> b) DESC` is a
sequential scan and a sort.

The index is HNSW rather than IVFFlat. IVFFlat is trained on existing rows to
pick its lists, so an index built over an empty table — which is exactly when
this store is created — is a bad index, and somebody has to remember to rebuild
it later. HNSW needs no training pass, so an empty table indexes correctly and
stays correct as rows arrive.

## The critical path

`search` is awaited once per turn, before the person who spoke hears anything.

`PgVector` therefore imposes its own three-second deadline, well short of the
HTTP read timeouts elsewhere in a deployment, and **an expired deadline returns
`Ok(vec![])` rather than an error**. A slow store should cost the turn a pause
and no memory — not a pause *and* a warning about a failure the person cannot
hear anyway. Lower it with `PgVectorBuilder::deadline`.

Empty results are always `Ok(Vec::new())`, never an error. The first turn of
every conversation legitimately finds nothing, and an error there would make the
runtime emit a spurious warning every single time.

## Builtin

### Persistence is opt-in

`BuiltinBuilder::path` defaults to absent, and absent means **genuinely
ephemeral** — records live in the process and die with it. Nothing is written
anywhere.

That default is deliberate. A memory store holds what people said to the
assistant, and one that silently began recording transcripts to disk because disk
was the easier default would be a surprise of the worst kind.

With a path, the in-memory structure is identical and every write additionally
dumps the whole set beside the target and renames it — the same
write-temp-then-rename `conduit_store::FileStore` uses, reused rather than
reinvented, so a crash mid-write leaves the previous file intact rather than a
truncated one. The file is read once at construction, and a file that will not
parse is an `Error::Config` from `build()` rather than an empty store: starting
fresh would be silent data loss.

### Health

| Configuration | Health |
| --- | --- |
| No path | `Healthy` — there is nothing to reach and nothing that can be wrong |
| Path whose directory is writable | `Healthy` |
| Path whose directory is not writable | `Degraded`, with the reason |

Degraded rather than `Unhealthy`, because the in-process half still stores and
still recalls for as long as the process lives, which is most of what this store
does. Reporting it unhealthy would take a store doing most of its job out of
service over the half that is broken. For the same reason an unwritable directory
does not stop `build()` from succeeding — a server that will not start because an
optional file cannot be written has turned a degraded capability into an outage.

### Capacity

`DEFAULT_CAPACITY` is 1000, and the oldest record is dropped first. A capacity of
zero is refused at build time: it describes a store that accepts every write and
remembers nothing, which would otherwise present as a store that never recalls.

## PgVector

### Two decisions, and why

#### The extension is attempted at construction, not in the shared migrator

`CREATE EXTENSION IF NOT EXISTS vector`, the embedding column, and the HNSW index
are all attempted in `from_pool`, and a failure is a warning plus a `Degraded`
health report rather than an error.

A migration in the shared migrator maps a failure to `Error::Config` at startup,
which would stop the **whole server booting** over an optional capability — on a
deployment whose database may simply not offer the extension and whose operator
may not have the rights to install it. That contradicts how this codebase already
treats optional things: a failing store is reported and stepped over, and a
provider that will not build is reported rather than panicked, because a running
server should say so and keep serving the rest.

So one build serves both a plain PostgreSQL and a pgvector one. On the plain one
the store runs in `Mode::Keyword`: rows are stored without embeddings and
retrieval is **BM25 over the 500 most recent candidates**, reusing the same
tokeniser and ranking `Builtin` uses rather than `to_tsvector` — so a deployment
without the extension gets one documented behaviour rather than a third one.

Every reference to the `vector` type, the `<=>` operator, and the
`vector_cosine_ops` operator class is schema-qualified with wherever the
extension turned out to live. An extension is database-global while its objects
live in one schema, and `CREATE EXTENSION` puts them in the first schema on the
search path — so a deployment whose search path is not that schema cannot see the
type at all, and would silently degrade to keyword on a database that has
pgvector installed.

#### A speaker-scoped record with no speaker is stored as shared

The filter is `speaker IS NULL OR speaker = $n`, in both backends: a record
stored with no speaker matches **every** speaker's query.

Refusing the write instead would mean a pipeline bound to `scope: speaker` with
nothing identifying the speaker silently discarded every single turn, with no
signal but a store that never remembers anything. The runtime has no speaker
identification provider today, so that is not a hypothetical — it is what such a
binding produces.

This is a **privacy-shaped default and must not be discovered**. If speaker
identification is not wired up, `scope: speaker` records are readable by everyone
the deployment serves.

### Schema

Columns, not `jsonb` — the only stored thing in this workspace that is shredded,
for exactly the reason the others are not. Nothing queries inside a pipeline
graph; *everything* queries inside a memory record, and none of the filtering or
ordering could use an index through a blob.

| Column | Type | Notes |
| --- | --- | --- |
| `id` | `UUID` | Primary key. |
| `provider` | `TEXT` | Filtered on in **every** query. |
| `content` | `TEXT` | |
| `scope` | `TEXT` | Serde's snake_case spelling, taken from serde rather than written twice. |
| `conversation` | `UUID` | Nullable. |
| `speaker` | `UUID` | Nullable — the runtime usually has none. |
| `metadata` | `JSONB` | Nullable; the runtime always writes null. |
| `created_at` | `TIMESTAMPTZ` | The retention story's ordering key. |
| `embedding` | `vector(n)` | Nullable, so the degraded path can still store. |

The `provider` column exists because two provider definitions may both be
pgvector stores against one database — a household one and a personal one — and
must not see each other's records. It is the descriptor's id, so renaming a
definition orphans its records.

### Health

| Situation | Health |
| --- | --- |
| Database unreachable | `Unhealthy` |
| No `pgvector` | `Degraded` — keyword ranking, with the reason |
| Embedder has failed at least once | `Degraded` — rows since then have no embedding |
| Otherwise | `Healthy` |

Degraded whenever the store can still store and still return something. A store
retrieving by keyword is worse than one retrieving by meaning and much better
than none. Only a database it cannot reach is unhealthy, because that is the only
case where it can do nothing at all.

A failed embedding stores the row anyway, with a null `embedding`. Losing the
exchange entirely over a dependency needed only for *retrieval* would be worse,
and the row is still findable by keyword and can be re-embedded later.

## Embeddings

`PgVector` takes an `Arc<dyn Embedder>`. The implementation for a real
deployment is `conduit_openai::OpenAiEmbeddings`, wrapped by `OpenAiEmbedder`
behind this crate's `openai` feature — `POST {base_url}/embeddings`, served by the
hosted API and by Ollama, vLLM, LM Studio, and `text-embeddings-inference`.

It is a **plain struct, not a `Provider`**. There is no embedding capability and
there should not be one: a capability is something an operator binds to a
pipeline node, and nobody binds "turn text into a vector" to a node. It is a
dependency of whatever needed the vector. Adding
`ProviderCapability::Embedding` would mean touching the capability enum, every
mapping function over it, the registry, and the operator console, to surface
something no graph can use.

`Embedder::dimensions` is supplied rather than discovered, because the
`vector(n)` column has to be declared before the first embedding exists.

## What the runtime actually stores

Read from `conduit_runtime::turn`, because it shapes everything above:

- **One concatenated blob per turn.** The runtime stores
  `"Asked: {question}\nAnswered: {answer}"` as a single record. BM25 therefore
  shares one document and one length normalisation across the question and the
  answer, and an embedding spans two semantically different texts. A long answer
  dilutes the question's terms.
- **Retrieval happens once per turn**, before the first model call, and
  `Match.score` is **discarded** — only `record.content` is used. The ordering
  matters; the numbers do not reach the model.
- **`metadata` is always null.**
- **Nothing calls `forget_conversation`.** Conversation-scoped records outlive
  their conversations. `Builtin`'s capacity bounds the damage; the pgvector table
  grows without limit.

Both backends implement `forget_conversation` correctly anyway.

### Retention

Because nothing calls it, a pgvector deployment needs a cleanup job. The
`memory_records_provider_created_at` index exists for this:

```sql
-- Records older than 90 days, for one store.
DELETE FROM memory_records
WHERE provider = 'household'
  AND created_at < now() - INTERVAL '90 days';

-- Or only the conversation-scoped ones, which were never meant to outlive
-- their conversation in the first place.
DELETE FROM memory_records
WHERE provider = 'household'
  AND scope = 'conversation'
  AND created_at < now() - INTERVAL '7 days';
```

## Security

Record content is never logged at any level, and a credential never is: the
`Debug` impl on `PgVector` is hand-written partly because an embedder holds a
configured HTTP client that may hold an API key, and a derived impl would be one
`dbg!` away from printing it. The embedding adapter logs a text's length, not its
text.

Everything reaching SQL does so as a bound parameter. Three things are formatted
into statement text, all audited: the table and column names are compile-time
literals; the vector width is a `usize` this build read off the configured
embedder, so it can only produce digits; and the extension's schema is quoted by
PostgreSQL's own `quote_ident` before it is used. The embedding itself is bound as
one parameter and cast in SQL, so the numbers never become statement text.

## Tests

`Builtin`'s tests need nothing and never skip.

The pgvector tests skip unless `CONDUIT_TEST_POSTGRES_URL` names a database,
because a test that silently passes without the thing it is testing is worse than
no test at all. Each test creates its own schema.

```sh
CONDUIT_TEST_POSTGRES_URL=postgres://localhost/conduit_test \
  cargo test -p conduit-memory --features postgres
```

The vector cases skip a **second** time, and just as visibly, when the database
is reachable but has no `pgvector` — that is the same distinction the store draws
at construction, and a plain PostgreSQL is a configuration this crate is expected
to serve. The keyword-degraded cases are the ones that must pass there, and they
do. Running the suite against both kinds of database is how both paths are
covered; neither run alone exercises everything.

## Accepted Limitations

- **The stored blob is one document.** A question and its answer share one length
  normalisation and one embedding. Splitting them would rank better and is a
  runtime change, not a change here.
- **A tiny corpus ranks by surprise rather than by aboutness.** With two or three
  records, a word that happens to appear in only one looks rare and can outrank
  the word the caller cared about. It corrects itself as records accumulate.
  Pinned by a test so it is documented rather than discovered.
- **No stemming.** "recycling" does not match "recycle". The cost of not assuming
  a language.
- **`Builtin` rewrites its whole file on every write.** Fine at a thousand short
  records; an append log would need compaction, a reader tolerant of a torn last
  line, and its own tests.
- **`Builtin` rescores every candidate per search.** Fine at this scale. The fix,
  if it stops being fine, is an inverted index.
- **A degraded pgvector search only sees the 500 most recent candidates.** It
  cannot rank in the database, so it ranks what it can carry. Older records are
  invisible to a degraded search.
- **Renaming a provider definition orphans its records.** The `provider` column
  is the descriptor's id, and the alternative is a second identity nobody can
  see.
- **`embedding_failed` is never cleared.** A store whose embedder failed once
  reports itself degraded until it is rebuilt. One transient failure means some
  rows have no embedding, and that stays true.
- **A record stored while degraded is invisible to vector search** even after the
  extension arrives, until something re-embeds it. Nothing does yet.
- **The pgvector table grows without limit.** See Retention above.
