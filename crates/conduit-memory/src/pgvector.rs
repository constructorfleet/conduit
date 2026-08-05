//! Memory in PostgreSQL, retrieved by vector similarity.
//!
//! Shares the deployment's database rather than adding a second one: a
//! deployment already running PostgreSQL for its pipelines has the hard part of
//! operating a database solved, and a dedicated vector service would be a
//! second thing to back up, upgrade, and reach.
//!
//! # Cosine distance, and an HNSW index
//!
//! Similarity is cosine, `1 - (embedding <=> query)`, because embedding models
//! are trained for it and because it ignores vector magnitude — which for an
//! embedding is an artefact of length rather than of meaning. pgvector's `<=>`
//! is cosine *distance* in `0..2`, so the similarity is `1 - distance`, and a
//! negative result is clamped to `0.0`: a record pointing away from the query is
//! irrelevant, not negatively relevant, and [`Match::score`] is documented as
//! `0.0..=1.0`.
//!
//! The ordering is by the raw `<=>` operator and not by the computed similarity,
//! because the index is only usable in that form — `ORDER BY 1 - (a <=> b) DESC`
//! is a sequential scan and a sort.
//!
//! The index is HNSW rather than IVFFlat. IVFFlat has to be trained on existing
//! rows to pick its lists, so an index built over an empty table — which is
//! exactly when this store is created — is a bad index, and somebody has to
//! remember to rebuild it later. HNSW needs no training pass, so an empty table
//! indexes correctly and stays correct as rows arrive.
//!
//! # Columns, not a document
//!
//! Every other stored thing in this workspace is `jsonb`, and this one is not,
//! for the reason the others are: nothing queries inside a pipeline graph, and
//! *everything* queries inside a memory record. The scope, the conversation, the
//! speaker, and the embedding are all filtered or ordered on, and none of that
//! can use an index through a blob.
//!
//! # Sharing one database
//!
//! Every query filters on a `provider` column. Two provider definitions may both
//! be pgvector stores against one database — a personal one and a household one,
//! say — and each must see only its own records.
//!
//! [`Match::score`]: conduit_provider::memory::Match::score

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use conduit_core::id::{ConversationId, SpeakerId};
use conduit_core::memory::Scope;
use conduit_core::{Error, Result};
use conduit_provider::memory::{Match, Memory, Query, Record};
use conduit_provider::{Capability, Descriptor, Health, Provider};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

use crate::embed::Embedder;
use crate::{bm25, SEARCH_DEADLINE};

/// The table both modes read and write.
///
/// One table for every pgvector store in a deployment, partitioned by the
/// `provider` column rather than by name: a table per provider would mean DDL
/// on every definition an operator saves.
const TABLE: &str = "memory_records";

/// How long to wait for a connection before giving up.
///
/// The same ten seconds [`conduit_store`] uses, and for the same reason: long
/// enough to ride out a database restart, short enough that a wrong URL is a
/// clear failure rather than a hang.
///
/// [`conduit_store`]: https://docs.rs/conduit-store
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// How many connections one store keeps.
const MAX_CONNECTIONS: u32 = 5;

/// How many rows the keyword-degraded path pulls back to rank in this process.
///
/// The degraded path cannot rank in the database, so it ranks the most recent
/// candidates instead. Five hundred is enough that a day's conversation is
/// entirely within reach and small enough that the transfer is not felt on a
/// turn. Anything older is invisible to a degraded search — which is one more
/// reason to install the extension.
const KEYWORD_CANDIDATES: i64 = 500;

/// How a store retrieves, once construction has found out what it can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `pgvector` is installed: retrieval is by cosine distance.
    Vector,
    /// `pgvector` is not available: retrieval is BM25 over recent candidates.
    ///
    /// The same ranking [`Builtin`](crate::Builtin) uses, so a deployment that
    /// cannot install the extension gets a store that behaves like the built-in
    /// one with a shared, durable table behind it.
    Keyword,
}

/// Builds a [`PgVector`] store.
pub struct PgVectorBuilder {
    id: String,
    label: Option<String>,
    embedder: Arc<dyn Embedder>,
    deadline: Duration,
}

impl PgVectorBuilder {
    /// Sets the human-readable name operator screens show.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Replaces the deadline a search is abandoned after.
    ///
    /// Defaults to [`SEARCH_DEADLINE`]. Lower it for a deployment that would
    /// rather never pause a turn than ever recall anything slowly.
    #[must_use]
    pub const fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Connects to `url` and prepares the schema.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the database cannot be reached or the table
    /// cannot be created. A *missing `pgvector` extension* is not an error: see
    /// [`PgVectorBuilder::from_pool`].
    pub async fn connect(self, url: &str) -> Result<PgVector> {
        let pool = PgPoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect(url)
            .await
            .map_err(|error| Error::Config(format!("cannot reach the database: {error}")))?;
        self.from_pool(pool).await
    }

    /// Uses an existing pool, preparing the schema.
    ///
    /// The pool a deployment already has, so a memory store adds no second set
    /// of connections to the same database.
    ///
    /// # Schema, at construction rather than in a migration
    ///
    /// The table and its plain indexes are created here, and so is the attempt
    /// at `CREATE EXTENSION IF NOT EXISTS vector` and the embedding column and
    /// HNSW index that depend on it.
    ///
    /// The extension deliberately does *not* go in the shared migrator. A
    /// migration failure there becomes an [`Error::Config`] at startup, which
    /// stops the whole server booting — over an optional capability, on a
    /// deployment whose database may simply not have the extension available and
    /// whose operator may not have the rights to install it. This codebase
    /// already treats optional things the other way: a failing store is reported
    /// and stepped over, and a provider that will not build is reported rather
    /// than panicked, because a running server should say so and keep serving
    /// the rest. So the attempt is made here, a failure is warned about and
    /// reported through [`Provider::health`], and the store runs in
    /// [`Mode::Keyword`]. One build serves a plain PostgreSQL and a pgvector one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the table or its plain indexes cannot be
    /// created — without them the store cannot do anything at all, and a
    /// provider that cannot be built is reported by the runtime rather than
    /// panicked over.
    pub async fn from_pool(self, pool: PgPool) -> Result<PgVector> {
        PgVector::create_table(&pool).await?;

        let dimensions = self.embedder.dimensions();
        let vector_schema = match PgVector::enable_vectors(&pool, dimensions).await {
            Ok(schema) => Some(schema),
            Err(error) => {
                // Warned rather than returned: see the note above. The reason is
                // kept so `health` can report it instead of an operator having
                // to find this line in the log.
                tracing::warn!(
                    store = %self.id,
                    %error,
                    "pgvector is not available; ranking memory by keyword instead"
                );
                None
            }
        };

        let degraded = vector_schema.is_none().then(|| {
            format!(
                "`pgvector` is not installed, so memory is ranked by keyword over the \
                 {KEYWORD_CANDIDATES} most recent records rather than by meaning"
            )
        });

        let label = self.label.unwrap_or_else(|| self.id.clone());
        Ok(PgVector {
            descriptor: Descriptor::new(self.id.clone(), Capability::Memory)
                .with_label(label)
                .with_version(env!("CARGO_PKG_VERSION")),
            provider: self.id,
            pool,
            embedder: self.embedder,
            vector_schema,
            degraded,
            deadline: self.deadline,
            embedding_failed: AtomicBool::new(false),
        })
    }
}

/// Memory in PostgreSQL, retrieved by vector similarity.
pub struct PgVector {
    descriptor: Descriptor,
    /// The value of the `provider` column for this store's rows.
    ///
    /// The descriptor's id, so the rows a store owns are the rows filed under
    /// the name a pipeline selects it by. Renaming a definition therefore
    /// orphans its records, which is documented rather than fixed: the
    /// alternative is a second identity nobody can see.
    provider: String,
    pool: PgPool,
    embedder: Arc<dyn Embedder>,
    /// The already-quoted schema `pgvector` lives in, when it is installed.
    ///
    /// `None` is [`Mode::Keyword`]. `Some` is the qualifier every reference to
    /// the `vector` type and the `<=>` operator carries: an extension is
    /// database-global while its objects live in one schema, so a deployment
    /// whose search path is not that schema cannot see the type unqualified.
    vector_schema: Option<String>,
    /// Why this store is degraded, if it is.
    degraded: Option<String>,
    deadline: Duration,
    /// Whether the embedder has failed since the store was built.
    ///
    /// Only ever set, never cleared: it exists so that a store whose embedding
    /// endpoint has gone away reports itself degraded, and one transient failure
    /// is worth an operator's attention even if the next call succeeds.
    embedding_failed: AtomicBool,
}

impl std::fmt::Debug for PgVector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-written because `dyn Embedder` is not `Debug`, and because an
        // embedder holds a configured HTTP client that may hold an API key: the
        // derived form would be one `dbg!` away from printing a credential.
        formatter
            .debug_struct("PgVector")
            .field("provider", &self.provider)
            .field("mode", &self.mode())
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

impl PgVector {
    /// A builder for a store identified as `id`, embedding with `embedder`.
    ///
    /// `id` is both what a pipeline selects and the value of the `provider`
    /// column, so it must be stable across releases.
    #[must_use]
    pub fn builder(id: impl Into<String>, embedder: Arc<dyn Embedder>) -> PgVectorBuilder {
        PgVectorBuilder { id: id.into(), label: None, embedder, deadline: SEARCH_DEADLINE }
    }

    /// The underlying pool, for callers that need their own queries.
    ///
    /// Public for the same reason [`conduit_store`]'s is: the retention query
    /// this store documents but does not run is one somebody has to be able to
    /// issue.
    ///
    /// [`conduit_store`]: https://docs.rs/conduit-store
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// How this store retrieves.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        match self.vector_schema {
            Some(_) => Mode::Vector,
            None => Mode::Keyword,
        }
    }

    /// Creates the table and the indexes that need no extension.
    ///
    /// `IF NOT EXISTS` throughout, because every replica runs this at startup:
    /// the second one must be a no-op rather than a crash loop.
    async fn create_table(pool: &PgPool) -> Result<()> {
        // Every statement here is a compile-time literal — no identifier, value,
        // or predicate is interpolated from anything a caller supplied.
        for statement in [
            "CREATE TABLE IF NOT EXISTS memory_records (
                 id           UUID PRIMARY KEY,
                 provider     TEXT        NOT NULL,
                 content      TEXT        NOT NULL,
                 scope        TEXT        NOT NULL,
                 conversation UUID,
                 speaker      UUID,
                 metadata     JSONB,
                 created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
             )",
            // Every query filters on the provider and most also filter on the
            // conversation, so this is the index that makes the filters cheap.
            "CREATE INDEX IF NOT EXISTS memory_records_provider_conversation
                 ON memory_records (provider, conversation)",
            // `forget_conversation` and the documented retention query both
            // reach for a conversation directly.
            "CREATE INDEX IF NOT EXISTS memory_records_conversation
                 ON memory_records (conversation)",
            // The retention story: this is what lets an operator delete records
            // older than a cutoff without a sequential scan.
            "CREATE INDEX IF NOT EXISTS memory_records_provider_created_at
                 ON memory_records (provider, created_at DESC)",
        ] {
            sqlx::query(statement).execute(pool).await.map_err(|error| {
                Error::Config(format!("cannot prepare the `{TABLE}` table: {error}"))
            })?;
        }
        Ok(())
    }

    /// Installs `pgvector` and the column and index that need it.
    ///
    /// Returns the schema the extension lives in, already quoted for use as an
    /// identifier. Every later reference to the `vector` type, the `<=>`
    /// operator, and the `vector_cosine_ops` operator class is qualified with
    /// it, because an extension is database-global while its objects live in one
    /// schema: `CREATE EXTENSION` puts them in the first schema on the search
    /// path, so a deployment whose search path is not where the extension was
    /// originally installed cannot see the type at all. Qualifying is the only
    /// way to be right regardless of where a DBA put it.
    ///
    /// # Errors
    ///
    /// Returns an error if the extension is unavailable or the caller lacks the
    /// rights to create it, which is the ordinary case on a managed database
    /// that does not offer it. The caller degrades rather than failing.
    async fn enable_vectors(pool: &PgPool, dimensions: usize) -> Result<String> {
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(pool)
            .await
            .map_err(Self::failure)?;

        // Quoted by the database rather than by this crate: `quote_ident` is
        // PostgreSQL's own escaping for exactly this, so the one identifier
        // interpolated below was escaped by the thing that will parse it.
        let (schema,): (String,) = sqlx::query_as(
            "SELECT quote_ident(n.nspname)
             FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
             WHERE e.extname = 'vector'",
        )
        .fetch_one(pool)
        .await
        .map_err(Self::failure)?;

        // The width is a number this build computed from the configured
        // embedder's `dimensions`, not a string from a request, and a type
        // modifier cannot be a bind parameter. Formatted through `{}` on a
        // `usize`, so the only thing it can produce is digits.
        let column = format!(
            "ALTER TABLE memory_records
                 ADD COLUMN IF NOT EXISTS embedding {schema}.vector({dimensions})"
        );
        sqlx::query(sqlx::AssertSqlSafe(column)).execute(pool).await.map_err(Self::failure)?;

        // `vector_cosine_ops` because `<=>` is the operator the queries use, and
        // an index built for a different operator class is simply not used.
        let index = format!(
            "CREATE INDEX IF NOT EXISTS memory_records_embedding
                 ON memory_records USING hnsw (embedding {schema}.vector_cosine_ops)"
        );
        sqlx::query(sqlx::AssertSqlSafe(index)).execute(pool).await.map_err(Self::failure)?;
        Ok(schema)
    }

    /// Wraps a query failure.
    fn failure(error: sqlx::Error) -> Error {
        Error::provider("pgvector-memory", error)
    }

    /// A scope in the spelling the column holds.
    ///
    /// Taken from serde rather than written out again, so the column and the
    /// wire format cannot drift apart: they are the same three words by
    /// construction instead of by two lists agreeing.
    fn scope_text(scope: Scope) -> String {
        match serde_json::to_value(scope) {
            Ok(serde_json::Value::String(text)) => text,
            // `Scope` is a plain unit-variant enum with `rename_all`, so this is
            // unreachable. Falling back to `Debug` keeps it from being a panic
            // in the one place a panic would cost a turn.
            _ => format!("{scope:?}").to_lowercase(),
        }
    }

    /// A scope read back out of the column.
    fn scope_from(text: &str) -> Result<Scope> {
        serde_json::from_value(serde_json::Value::String(text.to_owned())).map_err(|error| {
            // A row that will not decode is broken, not absent — the same
            // stance the pipeline store takes, and here it also means the
            // scope filtering cannot be trusted for this row.
            Error::Config(format!(
                "a remembered record in `{TABLE}` has scope `{text}`, which this build does \
                 not recognise: {error}"
            ))
        })
    }

    /// A vector in the literal form `$n::vector` accepts.
    ///
    /// Bound as one parameter and cast in SQL, so the numbers never become part
    /// of the statement text. Written by hand rather than by adding the
    /// `pgvector` crate for a single conversion.
    fn vector_literal(embedding: &[f32]) -> String {
        let mut literal = String::with_capacity(embedding.len() * 8 + 2);
        literal.push('[');
        for (index, value) in embedding.iter().enumerate() {
            if index > 0 {
                literal.push(',');
            }
            // `{}` on an `f32` is finite-or-`NaN`, and a `NaN` is rejected by
            // the database rather than silently stored.
            literal.push_str(&value.to_string());
        }
        literal.push(']');
        literal
    }

    /// Rebuilds a record from one row.
    fn record_from(row: &sqlx::postgres::PgRow) -> Result<Record> {
        let scope: String = row.try_get("scope").map_err(Self::failure)?;
        let conversation: Option<uuid::Uuid> =
            row.try_get("conversation").map_err(Self::failure)?;
        let speaker: Option<uuid::Uuid> = row.try_get("speaker").map_err(Self::failure)?;
        let metadata: Option<serde_json::Value> =
            row.try_get("metadata").map_err(Self::failure)?;
        Ok(Record {
            content: row.try_get("content").map_err(Self::failure)?,
            scope: Self::scope_from(&scope)?,
            conversation: conversation.map(ConversationId::from_uuid),
            speaker: speaker.map(SpeakerId::from_uuid),
            // The column is nullable and the runtime always writes null, so an
            // absent metadata column reads back as the `Null` the record had.
            metadata: metadata.unwrap_or(serde_json::Value::Null),
        })
    }

    /// The `WHERE` clause every query shares, from `$2` onward.
    ///
    /// `$1` is reserved for the provider, which is never optional. The scope,
    /// conversation, and speaker predicates are always present in the text and
    /// always bound — a `NULL` parameter disables its own predicate — so there
    /// is one statement shape rather than eight, and the database can reuse a
    /// plan across turns.
    ///
    /// # A speaker-scoped record with no speaker
    ///
    /// `speaker IS NULL OR speaker = $4` — so a record stored with no speaker is
    /// shared: it answers every speaker's query. The runtime has no speaker
    /// identification today, so that is what a `scope: speaker` binding
    /// actually produces. Refusing such a write instead would mean the binding
    /// silently discarded every turn, with no signal but a store that never
    /// remembers anything. It is nonetheless a privacy-shaped default, which is
    /// why it is written down in three places rather than discovered once.
    const FILTER: &'static str = "provider = $1
         AND ($2::text IS NULL OR scope = $2)
         AND ($3::uuid IS NULL OR conversation IS NULL OR conversation = $3)
         AND ($4::uuid IS NULL OR speaker IS NULL OR speaker = $4)";

    /// Retrieves by cosine distance.
    ///
    /// `schema` is the already-quoted schema `pgvector` lives in.
    async fn search_by_vector(&self, query: &Query, schema: &str) -> Result<Vec<Match>> {
        let embedding = self.embedder.embed(&query.text).await?;
        // Rows stored while degraded have no embedding and cannot be compared,
        // so they are excluded rather than sorted arbitrarily. Ordered by the
        // raw `<=>` operator and not by the computed similarity, because the
        // HNSW index is only usable in that form: `ORDER BY 1 - (a <=> b) DESC`
        // is a sequential scan and a sort.
        //
        // The operator itself has to be reached through `OPERATOR(schema.<=>)`
        // for the same reason the type does — an operator is resolved through
        // the search path, and the extension's schema may not be on it.
        let statement = format!(
            "SELECT content, scope, conversation, speaker, metadata,
                    embedding OPERATOR({schema}.<=>) $5::{schema}.vector AS distance
             FROM {TABLE}
             WHERE {filter} AND embedding IS NOT NULL
             ORDER BY embedding OPERATOR({schema}.<=>) $5::{schema}.vector
             LIMIT $6",
            filter = Self::FILTER
        );

        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(&self.provider)
            .bind(query.scope.map(Self::scope_text))
            .bind(query.conversation.map(|id| *id.as_uuid()))
            .bind(query.speaker.map(|id| *id.as_uuid()))
            .bind(Self::vector_literal(&embedding))
            .bind(i64::try_from(query.limit).unwrap_or(i64::MAX))
            .fetch_all(&self.pool)
            .await
            .map_err(Self::failure)?;

        rows.iter()
            .map(|row| {
                let distance: f64 = row.try_get("distance").map_err(Self::failure)?;
                // `<=>` is a distance in `0..2`, so the similarity is
                // `1 - distance`. A record pointing away from the query is
                // irrelevant, not negatively relevant, so the negative half is
                // clamped away rather than rescaled — rescaling would give an
                // unrelated record a middling score.
                #[allow(clippy::cast_possible_truncation)]
                let score = (1.0 - distance).clamp(0.0, 1.0) as f32;
                Ok(Match { record: Self::record_from(row)?, score })
            })
            .collect()
    }

    /// Retrieves by BM25 over the most recent candidates.
    ///
    /// The degraded path, and the same ranking [`Builtin`](crate::Builtin) uses,
    /// so a deployment without the extension gets one behaviour rather than a
    /// third one. The database cannot rank this, so it supplies candidates —
    /// most recent first, capped at [`KEYWORD_CANDIDATES`] — and the ranking
    /// happens here.
    async fn search_by_keyword(&self, query: &Query) -> Result<Vec<Match>> {
        let statement = format!(
            "SELECT content, scope, conversation, speaker, metadata
             FROM {TABLE}
             WHERE {filter}
             ORDER BY created_at DESC
             LIMIT $5",
            filter = Self::FILTER
        );

        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(&self.provider)
            .bind(query.scope.map(Self::scope_text))
            .bind(query.conversation.map(|id| *id.as_uuid()))
            .bind(query.speaker.map(|id| *id.as_uuid()))
            .bind(KEYWORD_CANDIDATES)
            .fetch_all(&self.pool)
            .await
            .map_err(Self::failure)?;

        let records: Vec<Record> =
            rows.iter().map(Self::record_from).collect::<Result<Vec<_>>>()?;
        let documents: Vec<Vec<String>> =
            records.iter().map(|record| bm25::tokens(&record.content)).collect();

        // The rows arrived newest first and the sort is stable, so ties break by
        // recency exactly as they do in the built-in store.
        Ok(bm25::rank(&documents, &query.text, query.limit)
            .into_iter()
            .map(|(index, score)| Match { record: records[index].clone(), score })
            .collect())
    }
}

#[async_trait::async_trait]
impl Provider for PgVector {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Whether this store can serve.
    ///
    /// [`Health::Degraded`] rather than [`Health::Unhealthy`] whenever it can
    /// still store and still return something: a store retrieving by keyword is
    /// worse than one retrieving by meaning and much better than none. Only a
    /// database it cannot reach is unhealthy, because that is the case where it
    /// can do nothing whatsoever.
    async fn health(&self) -> Health {
        if let Err(error) = sqlx::query("SELECT 1").execute(&self.pool).await {
            return Health::Unhealthy { reason: format!("cannot reach the database: {error}") };
        }

        if let Some(reason) = &self.degraded {
            return Health::Degraded { reason: reason.clone() };
        }
        if self.embedding_failed.load(Ordering::Relaxed) {
            return Health::Degraded {
                reason: "the embedding endpoint has failed at least once; records stored \
                         since then have no embedding and are invisible to vector search"
                    .to_owned(),
            };
        }
        Health::Healthy
    }
}

#[async_trait::async_trait]
impl Memory for PgVector {
    /// Stores a record, with an embedding when one can be produced.
    ///
    /// A failed embedding stores the row anyway, with a null `embedding`. The
    /// alternative is losing the exchange entirely over a dependency that is
    /// only needed for *retrieval*, and a row without an embedding is still
    /// findable by the keyword path and can be re-embedded later. What it is not
    /// is findable by vector search, which is why it is reported through
    /// [`Provider::health`].
    async fn store(&self, record: Record) -> Result<()> {
        let embedding = match &self.vector_schema {
            None => None,
            Some(_) => match self.embedder.embed(&record.content).await {
                Ok(embedding) => Some(Self::vector_literal(&embedding)),
                Err(error) => {
                    self.embedding_failed.store(true, Ordering::Relaxed);
                    // The content is never logged: it is what somebody said.
                    tracing::warn!(
                        store = %self.provider,
                        %error,
                        "cannot embed a record; storing it without one, so it will only be \
                         found by keyword"
                    );
                    None
                }
            },
        };

        // Two statements rather than one with a cast on a possibly-null
        // parameter: `NULL::vector` is only a legal cast when the type exists,
        // and in `Mode::Keyword` it does not — there is no embedding column
        // either.
        let statement = match (&embedding, &self.vector_schema) {
            (Some(_), Some(schema)) => sqlx::AssertSqlSafe(format!(
                "INSERT INTO memory_records
                     (id, provider, content, scope, conversation, speaker, metadata, embedding)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8::{schema}.vector)"
            )),
            _ => sqlx::AssertSqlSafe(
                "INSERT INTO memory_records
                     (id, provider, content, scope, conversation, speaker, metadata)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)"
                    .to_owned(),
            ),
        };

        let mut insert = sqlx::query(statement)
            .bind(uuid::Uuid::new_v4())
            .bind(&self.provider)
            .bind(&record.content)
            .bind(Self::scope_text(record.scope))
            .bind(record.conversation.map(|id| *id.as_uuid()))
            .bind(record.speaker.map(|id| *id.as_uuid()))
            .bind(
                // The runtime always writes null. Storing SQL `NULL` rather
                // than the JSON literal `null` keeps "no metadata" one value
                // instead of two that compare unequal.
                (!record.metadata.is_null()).then(|| record.metadata.clone()),
            );
        if let Some(embedding) = embedding {
            insert = insert.bind(embedding);
        }

        insert.execute(&self.pool).await.map(|_| ()).map_err(Self::failure)
    }

    /// Retrieves the records best matching `query`, most relevant first.
    ///
    /// Bounded by this store's own deadline, and an expired deadline returns
    /// `Ok(Vec::new())` rather than an error. This is awaited on the critical
    /// path of every turn, before the person who spoke hears anything, and the
    /// HTTP read timeout on the embedding endpoint is far too long to hold a
    /// voice turn open. A store that is slow should cost the turn a pause and no
    /// memory — not a pause *and* a warning about a failure the person cannot
    /// hear anyway.
    ///
    /// Emptiness is likewise never an error: the first turn of every
    /// conversation legitimately finds nothing, and returning an error there
    /// would emit a spurious warning every single time.
    async fn search(&self, query: Query) -> Result<Vec<Match>> {
        if query.limit == 0 {
            return Ok(Vec::new());
        }

        let retrieval = async {
            let Some(schema) = &self.vector_schema else {
                return self.search_by_keyword(&query).await;
            };
            match self.search_by_vector(&query, schema).await {
                Ok(found) => Ok(found),
                // The embedder is reachable in principle and was not now.
                // Keyword over the same table is a worse answer than vector and
                // a much better one than none.
                Err(error) => {
                    self.embedding_failed.store(true, Ordering::Relaxed);
                    tracing::warn!(
                        store = %self.provider,
                        %error,
                        "cannot search by vector; falling back to keyword ranking"
                    );
                    self.search_by_keyword(&query).await
                }
            }
        };

        match tokio::time::timeout(self.deadline, retrieval).await {
            Ok(found) => found,
            Err(_) => {
                tracing::warn!(
                    store = %self.provider,
                    deadline_ms = self.deadline.as_millis(),
                    "memory retrieval did not finish in time; answering without it"
                );
                Ok(Vec::new())
            }
        }
    }

    /// Deletes everything stored for a conversation.
    ///
    /// Nothing in the runtime calls this today, which is why the retention query
    /// in the README exists: conversation-scoped records outlive their
    /// conversations, and unlike the built-in store this table has no capacity
    /// to bound the damage.
    async fn forget_conversation(&self, conversation: ConversationId) -> Result<()> {
        sqlx::query("DELETE FROM memory_records WHERE provider = $1 AND conversation = $2")
            .bind(&self.provider)
            .bind(conversation.as_uuid())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(Self::failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_is_stored_in_the_spelling_serde_writes() {
        // The column and the wire format are the same three words, and this is
        // what keeps them that way.
        for (scope, spelling) in [
            (Scope::Conversation, "conversation"),
            (Scope::Speaker, "speaker"),
            (Scope::Global, "global"),
        ] {
            assert_eq!(PgVector::scope_text(scope), spelling);
            assert_eq!(PgVector::scope_from(spelling).expect("round trips"), scope);
        }
    }

    #[test]
    fn a_scope_this_build_does_not_recognise_is_reported_rather_than_guessed() {
        let error = PgVector::scope_from("forever").expect_err("unknown");
        assert!(error.to_string().contains("forever"), "{error}");
    }

    #[test]
    fn a_vector_is_written_in_the_literal_form_the_cast_accepts() {
        assert_eq!(PgVector::vector_literal(&[1.0, -0.5, 0.25]), "[1,-0.5,0.25]");
        assert_eq!(PgVector::vector_literal(&[]), "[]");
    }

    #[test]
    fn every_shared_filter_predicate_binds_rather_than_interpolating() {
        // The one rule that matters for a store reachable from a graph an
        // operator edits: nothing a caller supplies becomes statement text.
        assert!(PgVector::FILTER.contains("$1"), "{}", PgVector::FILTER);
        assert!(PgVector::FILTER.contains("scope = $2"), "{}", PgVector::FILTER);
        assert!(
            PgVector::FILTER.contains("speaker IS NULL OR speaker = $4"),
            "a record with no speaker is shared: {}",
            PgVector::FILTER
        );
    }
}
