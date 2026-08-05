//! The pgvector store, against a real database.
//!
//! Skipped unless `CONDUIT_TEST_POSTGRES_URL` names one, because a test that
//! silently passes without the thing it is testing is worse than no test at all.
//! Point it at a throwaway database — each test drops and recreates its own
//! schema.
//!
//! ```sh
//! CONDUIT_TEST_POSTGRES_URL=postgres://localhost/conduit_test \
//!   cargo test -p conduit-memory --features postgres
//! ```
//!
//! The vector cases skip a second time, and just as visibly, when the database
//! is reachable but has no `pgvector` extension. That is not a workaround: it is
//! the same distinction the store itself draws at construction, and a plain
//! PostgreSQL is a configuration this crate is expected to serve. The
//! keyword-degraded cases are the ones that must pass on such a database, and
//! they run there.

#![cfg(feature = "postgres")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use conduit_core::id::{ConversationId, SpeakerId};
use conduit_core::memory::Scope;
use conduit_memory::pgvector::Mode;
use conduit_memory::{Embedder, PgVector};
use conduit_provider::memory::{Match, Memory, Query, Record};
use conduit_provider::{Health, Provider};
use sqlx::AssertSqlSafe;

/// How wide the test embeddings are. Small, because nothing here needs meaning.
const DIMENSIONS: usize = 4;

/// An embedder with no network, producing a vector from the text's own words.
///
/// Deterministic and crude: each of the four dimensions counts how many of a
/// fixed set of marker words the text contains. Two texts about recycling
/// therefore point the same way and a text about a cat does not, which is all a
/// test of the *plumbing* needs — asserting that a real embedding model places
/// two sentences near each other would be testing the model.
struct Markers {
    calls: AtomicUsize,
}

impl Markers {
    fn new() -> Arc<Self> {
        Arc::new(Self { calls: AtomicUsize::new(0) })
    }
}

/// The words each dimension counts.
const MARKERS: [&str; DIMENSIONS] = ["recycling", "cat", "dentist", "tuesday"];

#[async_trait::async_trait]
impl Embedder for Markers {
    async fn embed(&self, text: &str) -> conduit_core::Result<Vec<f32>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let lowered = text.to_lowercase();
        let mut vector: Vec<f32> =
            MARKERS.iter().map(|marker| lowered.matches(marker).count() as f32).collect();
        // A zero vector has no direction, so cosine distance against it is
        // undefined and pgvector says so. A small constant in the last slot
        // keeps every vector orientable without changing which is nearest.
        if vector.iter().all(|value| *value == 0.0) {
            vector[DIMENSIONS - 1] = 0.001;
        }
        Ok(vector)
    }

    fn dimensions(&self) -> usize {
        DIMENSIONS
    }
}

/// An embedder that always fails, for the store's degraded write path.
struct Broken;

#[async_trait::async_trait]
impl Embedder for Broken {
    async fn embed(&self, _text: &str) -> conduit_core::Result<Vec<f32>> {
        Err(conduit_core::Error::Config("no embedding endpoint".to_owned()))
    }

    fn dimensions(&self) -> usize {
        DIMENSIONS
    }
}

/// An embedder that never answers, for the store's deadline.
struct Slow;

#[async_trait::async_trait]
impl Embedder for Slow {
    async fn embed(&self, _text: &str) -> conduit_core::Result<Vec<f32>> {
        std::future::pending().await
    }

    fn dimensions(&self) -> usize {
        DIMENSIONS
    }
}

/// The database to test against, if one was named.
fn base_url() -> Option<String> {
    std::env::var("CONDUIT_TEST_POSTGRES_URL").ok().filter(|url| !url.is_empty())
}

/// A URL that puts every table in `schema`.
///
/// Tests run in parallel against one database, so each needs its own schema.
fn scoped_url(base: &str, schema: &str) -> String {
    let separator = if base.contains('?') { '&' } else { '?' };
    format!("{base}{separator}options=-c%20search_path%3D{schema}")
}

/// Creates an empty schema and returns a URL scoped to it.
///
/// `schema` is `&'static str` rather than `&str` so the claim below — that it is
/// never user input — is enforced by the compiler instead of by a comment. A
/// schema name cannot be a bind parameter, so it has to be interpolated.
async fn schema(schema: &'static str) -> Option<String> {
    let base = base_url()?;
    let admin = sqlx::postgres::PgPool::connect(&base).await.expect("connects");
    // Audited for injection, as `AssertSqlSafe` requires: every caller passes a
    // literal, which the `&'static str` above is what guarantees.
    sqlx::query(AssertSqlSafe(format!("DROP SCHEMA IF EXISTS {schema} CASCADE")))
        .execute(&admin)
        .await
        .expect("drops any leftovers");
    sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .expect("creates the schema");
    admin.close().await;
    Some(scoped_url(&base, schema))
}

/// Whether the database has `pgvector` available at all.
async fn has_vector_extension() -> bool {
    let Some(base) = base_url() else { return false };
    let Ok(pool) = sqlx::postgres::PgPool::connect(&base).await else { return false };
    let available: Option<(String,)> =
        sqlx::query_as("SELECT name FROM pg_available_extensions WHERE name = 'vector'")
            .fetch_optional(&pool)
            .await
            .unwrap_or(None);
    pool.close().await;
    available.is_some()
}

/// Builds a store in its own schema, or skips the test with a visible note.
macro_rules! store_or_skip {
    ($schema:literal, $embedder:expr) => {
        match schema($schema).await {
            Some(url) => PgVector::builder("test-memory", $embedder)
                .connect(&url)
                .await
                .expect("connects and prepares"),
            None => {
                eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
                return;
            }
        }
    };
}

/// Builds a store that must be in [`Mode::Vector`], or skips visibly.
///
/// Two reasons to skip, reported separately, because they are two different
/// things to go and fix.
macro_rules! vector_store_or_skip {
    ($schema:literal, $embedder:expr) => {{
        if base_url().is_none() {
            eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
            return;
        }
        if !has_vector_extension().await {
            eprintln!(
                "skipped: the database at CONDUIT_TEST_POSTGRES_URL has no `pgvector` \
                 extension available, so vector retrieval cannot be exercised. The \
                 keyword-degraded cases cover what this database can do."
            );
            return;
        }
        let store = store_or_skip!($schema, $embedder);
        assert_eq!(
            store.mode(),
            Mode::Vector,
            "the extension is available, so the store must have used it"
        );
        store
    }};
}

/// A global record with no conversation and no speaker.
fn global(content: &str) -> Record {
    Record {
        content: content.to_owned(),
        scope: Scope::Global,
        conversation: None,
        speaker: None,
        metadata: serde_json::Value::Null,
    }
}

/// The contents of every match, most relevant first.
fn contents(found: &[Match]) -> Vec<&str> {
    found.iter().map(|item| item.record.content.as_str()).collect()
}

#[tokio::test]
async fn a_record_near_the_question_is_recalled_ahead_of_one_that_is_not() {
    let store = vector_store_or_skip!("vector_ranking", Markers::new());
    store.store(global("the cat is called mabel")).await.expect("stores");
    store.store(global("the recycling goes out on tuesday")).await.expect("stores");

    let found = store.search(Query::new("recycling", 5)).await.expect("searches");

    assert_eq!(
        contents(&found).first(),
        Some(&"the recycling goes out on tuesday"),
        "{found:?}"
    );
}

#[tokio::test]
async fn every_score_lands_between_zero_and_one() {
    // `<=>` is a distance in `0..2`, so `1 - distance` can be negative — and a
    // record pointing away from the question is irrelevant, not negatively
    // relevant.
    let store = vector_store_or_skip!("vector_scores", Markers::new());
    for content in ["recycling recycling recycling", "the cat is called mabel", "dentist"] {
        store.store(global(content)).await.expect("stores");
    }

    let found = store.search(Query::new("recycling", 10)).await.expect("searches");

    assert!(!found.is_empty());
    for item in &found {
        assert!((0.0..=1.0).contains(&item.score), "{item:?}");
    }
}

#[tokio::test]
async fn an_empty_store_reports_finding_nothing_rather_than_failing() {
    // The first turn of every conversation lands here.
    let store = store_or_skip!("vector_empty", Markers::new());

    let found = store.search(Query::new("recycling", 5)).await.expect("searches");

    assert!(found.is_empty());
}

#[tokio::test]
async fn a_query_naming_a_scope_does_not_recall_records_of_another() {
    let store = store_or_skip!("vector_scope", Markers::new());
    store.store(global("recycling is global")).await.expect("stores");
    store
        .store(Record { scope: Scope::Conversation, ..global("recycling per conversation") })
        .await
        .expect("stores");

    let found = store
        .search(Query { scope: Some(Scope::Global), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    assert_eq!(contents(&found), ["recycling is global"]);
}

#[tokio::test]
async fn a_query_naming_a_conversation_does_not_recall_another_conversation() {
    let mine = ConversationId::new();
    let store = store_or_skip!("vector_conversation", Markers::new());
    store
        .store(Record { conversation: Some(mine), ..global("recycling in mine") })
        .await
        .expect("stores");
    store
        .store(Record {
            conversation: Some(ConversationId::new()),
            ..global("recycling in theirs")
        })
        .await
        .expect("stores");

    let found = store
        .search(Query { conversation: Some(mine), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    assert_eq!(contents(&found), ["recycling in mine"]);
}

#[tokio::test]
async fn a_query_naming_a_speaker_does_not_recall_another_speakers_record() {
    let mine = SpeakerId::new();
    let store = store_or_skip!("vector_speaker", Markers::new());
    store
        .store(Record {
            scope: Scope::Speaker,
            speaker: Some(mine),
            ..global("recycling is mine")
        })
        .await
        .expect("stores");
    store
        .store(Record {
            scope: Scope::Speaker,
            speaker: Some(SpeakerId::new()),
            ..global("recycling is theirs")
        })
        .await
        .expect("stores");

    let found = store
        .search(Query { speaker: Some(mine), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    assert_eq!(contents(&found), ["recycling is mine"]);
}

#[tokio::test]
async fn a_speaker_scoped_record_with_no_speaker_is_recalled_by_every_speaker() {
    // `speaker IS NULL OR speaker = $4`. The runtime has no speaker
    // identification, so this is what a `scope: speaker` binding produces
    // today; refusing the write instead would silently discard every turn.
    let store = store_or_skip!("vector_shared_speaker", Markers::new());
    store
        .store(Record { scope: Scope::Speaker, ..global("recycling goes out tuesday") })
        .await
        .expect("stores");

    for _ in 0..2 {
        let found = store
            .search(Query { speaker: Some(SpeakerId::new()), ..Query::new("recycling", 5) })
            .await
            .expect("searches");
        assert_eq!(contents(&found), ["recycling goes out tuesday"]);
    }
}

#[tokio::test]
async fn two_stores_against_one_database_do_not_see_each_others_records() {
    // The whole reason there is a `provider` column, and why every query filters
    // on it: two definitions may both be pgvector stores against one database.
    let Some(url) = schema("vector_isolation").await else {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    };
    let mine =
        PgVector::builder("household", Markers::new()).connect(&url).await.expect("connects");
    let theirs =
        PgVector::builder("personal", Markers::new()).connect(&url).await.expect("connects");

    mine.store(global("recycling in the household store")).await.expect("stores");

    assert_eq!(
        contents(&mine.search(Query::new("recycling", 5)).await.expect("searches")).len(),
        1
    );
    assert!(
        theirs.search(Query::new("recycling", 5)).await.expect("searches").is_empty(),
        "the other store's records are not this store's"
    );
}

#[tokio::test]
async fn forgetting_a_conversation_leaves_every_other_record_alone() {
    let doomed = ConversationId::new();
    let kept = ConversationId::new();
    let store = store_or_skip!("vector_forget", Markers::new());
    store
        .store(Record { conversation: Some(doomed), ..global("recycling in the doomed one") })
        .await
        .expect("stores");
    store
        .store(Record { conversation: Some(kept), ..global("recycling in the kept one") })
        .await
        .expect("stores");
    store.store(global("recycling everywhere")).await.expect("stores");

    store.forget_conversation(doomed).await.expect("forgets");

    let found = store.search(Query::new("recycling", 10)).await.expect("searches");
    let recalled = contents(&found);
    assert!(!recalled.contains(&"recycling in the doomed one"), "{recalled:?}");
    assert!(recalled.contains(&"recycling in the kept one"), "{recalled:?}");
    assert!(recalled.contains(&"recycling everywhere"), "{recalled:?}");
}

#[tokio::test]
async fn forgetting_a_conversation_nothing_was_stored_for_succeeds() {
    let store = store_or_skip!("vector_forget_nothing", Markers::new());

    store.forget_conversation(ConversationId::new()).await.expect("forgets nothing");
}

#[tokio::test]
async fn forgetting_a_conversation_does_not_reach_another_stores_rows() {
    let Some(url) = schema("vector_forget_isolation").await else {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    };
    let conversation = ConversationId::new();
    let mine =
        PgVector::builder("household", Markers::new()).connect(&url).await.expect("connects");
    let theirs =
        PgVector::builder("personal", Markers::new()).connect(&url).await.expect("connects");
    theirs
        .store(Record { conversation: Some(conversation), ..global("recycling is theirs") })
        .await
        .expect("stores");

    mine.forget_conversation(conversation).await.expect("forgets its own");

    let found = theirs.search(Query::new("recycling", 5)).await.expect("searches");
    assert_eq!(contents(&found), ["recycling is theirs"]);
}

#[tokio::test]
async fn preparing_the_schema_twice_is_a_no_op_rather_than_a_failure() {
    // Every replica runs it at startup, so the second must not crash-loop.
    let Some(url) = schema("vector_idempotent").await else {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    };
    let first =
        PgVector::builder("test-memory", Markers::new()).connect(&url).await.expect("first");
    let second =
        PgVector::builder("test-memory", Markers::new()).connect(&url).await.expect("second");

    first.store(global("recycling goes out tuesday")).await.expect("stores");

    let found = second.search(Query::new("recycling", 5)).await.expect("searches");
    assert_eq!(contents(&found), ["recycling goes out tuesday"]);
}

#[tokio::test]
async fn a_record_is_shredded_into_columns_rather_than_stored_as_a_document() {
    // None of the filtering or ordering could use an index through a blob,
    // which is why this is the one stored thing in the workspace that is not
    // jsonb.
    let conversation = ConversationId::new();
    let store = store_or_skip!("vector_columns", Markers::new());
    store
        .store(Record {
            scope: Scope::Conversation,
            conversation: Some(conversation),
            ..global("recycling goes out tuesday")
        })
        .await
        .expect("stores");

    let row: (String, String, Option<uuid::Uuid>, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT content, scope, conversation, speaker FROM memory_records
         WHERE provider = 'test-memory'",
    )
    .fetch_one(store.pool())
    .await
    .expect("reads the columns directly");

    assert_eq!(row.0, "recycling goes out tuesday");
    // Snake case, the spelling serde writes, so an operator reading the table
    // by hand reads the same three words a graph names.
    assert_eq!(row.1, "conversation");
    assert_eq!(row.2, Some(*conversation.as_uuid()));
    assert_eq!(row.3, None, "the runtime usually has no speaker, so the column is nullable");
}

#[tokio::test]
async fn a_record_the_runtime_stored_keeps_its_null_metadata() {
    // The runtime always writes null, and it must read back as null rather than
    // as a JSON `null` that compares unequal.
    let store = store_or_skip!("vector_metadata", Markers::new());
    let stored = global("recycling goes out tuesday");
    store.store(stored.clone()).await.expect("stores");

    let found = store.search(Query::new("recycling", 5)).await.expect("searches");

    assert_eq!(found.first().map(|item| item.record.clone()), Some(stored));
}

#[tokio::test]
async fn a_store_whose_embedder_fails_stores_the_record_anyway() {
    // Losing the exchange entirely over a dependency needed only for retrieval
    // would be the worse outcome, and the row is still findable by keyword.
    let store = store_or_skip!("vector_broken_embedder", Arc::new(Broken));
    store.store(global("recycling goes out tuesday")).await.expect("stores anyway");

    let embeddings: (i64,) =
        sqlx::query_as("SELECT count(*) FROM memory_records WHERE provider = 'test-memory'")
            .fetch_one(store.pool())
            .await
            .expect("counts");
    assert_eq!(embeddings.0, 1, "the row is there");

    // And it is still retrievable, because the failed vector search falls back.
    let found = store.search(Query::new("recycling", 5)).await.expect("searches");
    assert_eq!(contents(&found), ["recycling goes out tuesday"]);
}

#[tokio::test]
async fn a_store_whose_embedder_has_failed_reports_itself_degraded() {
    let store = store_or_skip!("vector_broken_health", Arc::new(Broken));
    store.store(global("recycling goes out tuesday")).await.expect("stores anyway");

    match store.health().await {
        Health::Degraded { reason } => assert!(!reason.is_empty(), "a reason is given"),
        // A database without the extension is degraded for that reason instead,
        // which is equally correct and is what this asserts either way.
        other => panic!("expected a degraded store, got {other:?}"),
    }
    assert!(store.health().await.is_usable(), "degraded still serves");
}

#[tokio::test]
async fn a_search_that_does_not_finish_in_time_recalls_nothing_rather_than_failing() {
    // The critical path of every turn. A slow store should cost the person a
    // pause and no memory, not a pause and a warning about a failure they
    // cannot hear anyway.
    let Some(url) = schema("vector_deadline").await else {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    };
    let store = PgVector::builder("test-memory", Arc::new(Slow))
        .deadline(Duration::from_millis(50))
        .connect(&url)
        .await
        .expect("connects");

    let started = std::time::Instant::now();
    let found = store.search(Query::new("recycling", 5)).await.expect("does not fail");

    assert!(found.is_empty());
    assert!(started.elapsed() < Duration::from_secs(1), "it gave up promptly");
}

#[tokio::test]
async fn a_limit_of_zero_recalls_nothing_without_asking_the_database() {
    let store = store_or_skip!("vector_zero_limit", Arc::new(Slow));

    let found = store.search(Query::new("recycling", 0)).await.expect("searches");

    assert!(found.is_empty(), "and it did not wait on the embedder to find out");
}

#[tokio::test]
async fn a_reachable_database_without_the_extension_still_stores_and_searches() {
    // The entire point of attempting the extension at construction rather than
    // in the shared migrator: one build serves a plain PostgreSQL and a
    // pgvector one, and on the plain one the store works.
    let Some(base) = base_url() else {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    };
    if has_vector_extension().await {
        eprintln!(
            "skipped: the database at CONDUIT_TEST_POSTGRES_URL has `pgvector`, so the \
             degraded path cannot be reached by configuration alone. Point this at a plain \
             PostgreSQL to exercise it."
        );
        return;
    }
    let _ = base;
    let store = store_or_skip!("keyword_degraded", Markers::new());

    assert_eq!(store.mode(), Mode::Keyword, "no extension, so keyword ranking");
    store.store(global("the cat is called mabel")).await.expect("stores");
    store.store(global("the recycling goes out on tuesday")).await.expect("stores");

    let found = store.search(Query::new("recycling collection", 5)).await.expect("searches");

    // Ranked first rather than returned alone: both records contain "the", and
    // with no stopword list — a language assumption this crate declines to make
    // — "the" is a term like any other. BM25 gives it almost no weight because
    // it is in every document, which is the mechanism that replaces a stopword
    // list rather than an accident.
    assert_eq!(
        contents(&found).first(),
        Some(&"the recycling goes out on tuesday"),
        "BM25 over the recent candidates, the same ranking the built-in store uses: {found:?}"
    );
    for item in &found {
        assert!((0.0..=1.0).contains(&item.score), "{item:?}");
    }
}

#[tokio::test]
async fn a_store_without_the_extension_says_why_it_is_degraded() {
    if base_url().is_none() {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    }
    if has_vector_extension().await {
        eprintln!(
            "skipped: the database at CONDUIT_TEST_POSTGRES_URL has `pgvector`, so it is \
             not degraded. Point this at a plain PostgreSQL to exercise it."
        );
        return;
    }
    let store = store_or_skip!("keyword_degraded_health", Markers::new());

    match store.health().await {
        Health::Degraded { reason } => {
            assert!(reason.contains("pgvector"), "{reason}");
            assert!(reason.contains("keyword"), "{reason}");
        }
        other => panic!("expected a degraded store, got {other:?}"),
    }
    assert!(store.health().await.is_usable());
}

#[tokio::test]
async fn a_store_without_the_extension_writes_rows_that_have_no_embedding() {
    if base_url().is_none() {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    }
    if has_vector_extension().await {
        eprintln!(
            "skipped: the database at CONDUIT_TEST_POSTGRES_URL has `pgvector`, so this \
             path is not taken. Point this at a plain PostgreSQL to exercise it."
        );
        return;
    }
    let store = store_or_skip!("keyword_no_embedding_column", Markers::new());
    store.store(global("recycling goes out tuesday")).await.expect("stores");

    // There is no embedding column at all, which is what makes the two-statement
    // insert necessary: `NULL::vector` is not a legal cast without the type.
    let columns: Vec<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns
         WHERE table_name = 'memory_records' AND column_name = 'embedding'",
    )
    .fetch_all(store.pool())
    .await
    .expect("reads the catalogue");
    assert!(columns.is_empty(), "no extension means no vector column");
}

#[tokio::test]
async fn the_extension_is_found_even_when_it_lives_off_the_search_path() {
    // Regression: `CREATE EXTENSION` puts its objects in the *first* schema on
    // the search path, and an extension is database-global. A deployment whose
    // search path is not the schema the extension was installed into therefore
    // cannot see the `vector` type at all — the store degraded to keyword on a
    // database that had pgvector installed and available. Every reference to the
    // type, the `<=>` operator, and the operator class is now qualified with
    // the schema the extension actually lives in.
    //
    // These tests each run in their own schema, so this is that exact
    // situation: the extension is in `public` and the table is not.
    let store = vector_store_or_skip!("vector_off_path", Markers::new());
    store.store(global("recycling goes out tuesday")).await.expect("stores");

    let found = store.search(Query::new("recycling", 5)).await.expect("searches");

    assert_eq!(contents(&found), ["recycling goes out tuesday"]);
    let stored: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM memory_records
         WHERE provider = 'test-memory' AND embedding IS NOT NULL",
    )
    .fetch_one(store.pool())
    .await
    .expect("counts");
    assert_eq!(stored.0, 1, "the row was stored with an embedding, not without one");
}

#[tokio::test]
async fn an_unreachable_database_fails_with_an_actionable_message() {
    // No server needed: nothing is listening on this port. Runs everywhere,
    // which is why it does not skip.
    let error = PgVector::builder("test-memory", Markers::new())
        .connect("postgres://127.0.0.1:1/nothing")
        .await
        .expect_err("cannot connect");

    assert!(error.to_string().contains("database"), "{error}");
}

#[tokio::test]
async fn a_store_reports_the_identity_it_was_built_with() {
    let Some(url) = schema("vector_identity").await else {
        eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
        return;
    };
    let store = PgVector::builder("household", Markers::new())
        .label("Household memory")
        .connect(&url)
        .await
        .expect("connects");

    assert_eq!(Provider::name(&store), "household");
    assert_eq!(store.descriptor().label, "Household memory");
    assert_eq!(store.descriptor().capability, conduit_provider::Capability::Memory);
}
