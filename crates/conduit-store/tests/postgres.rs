//! PostgreSQL, against a real database.
//!
//! These are skipped unless `CONDUIT_TEST_POSTGRES_URL` names one, because a
//! test that silently passes without the thing it is testing is worse than no
//! test at all. Point it at a throwaway database — each test drops and
//! recreates its own schema.
//!
//! ```sh
//! CONDUIT_TEST_POSTGRES_URL=postgres://localhost/conduit_test \
//!   cargo test -p conduit-store --features postgres
//! ```

#![cfg(feature = "postgres")]

use std::sync::Arc;

use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_provider::storage::PipelineStore;
use conduit_store::PostgresStore;

/// The database to test against, if one was named.
fn base_url() -> Option<String> {
    std::env::var("CONDUIT_TEST_POSTGRES_URL").ok().filter(|url| !url.is_empty())
}

/// A URL that puts every table in `schema`.
///
/// Tests run in parallel against one database, so each needs its own schema —
/// otherwise they truncate each other's rows and fail for reasons that have
/// nothing to do with the code.
fn scoped_url(base: &str, schema: &str) -> String {
    let separator = if base.contains('?') { '&' } else { '?' };
    format!("{base}{separator}options=-c%20search_path%3D{schema}")
}

/// Creates an empty schema and connects to it.
async fn store_in(schema: &str) -> Option<PostgresStore> {
    let base = base_url()?;
    let admin = sqlx::postgres::PgPool::connect(&base).await.expect("connects");
    // The schema name is a literal in this file, never user input.
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drops any leftovers");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("creates the schema");
    admin.close().await;

    Some(
        PostgresStore::connect(&scoped_url(&base, schema))
            .await
            .expect("connects and migrates"),
    )
}

/// Skips the test with a visible note when no database is configured.
macro_rules! store_or_skip {
    ($schema:literal) => {
        match store_in($schema).await {
            Some(store) => store,
            None => {
                eprintln!("skipped: set CONDUIT_TEST_POSTGRES_URL to run this");
                return;
            }
        }
    };
}

fn graph(name: &str) -> PipelineGraph {
    PipelineGraph::new(name)
        .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
        .with_node(Node::new("llm", NodeKind::Llm, "ollama"))
        .with_node(Node::new("tts", NodeKind::Tts, "piper"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

#[tokio::test]
async fn it_behaves_like_a_store() {
    // The same expectations the file and memory backends meet, so swapping
    // backends cannot change behaviour.
    let store: Arc<dyn PipelineStore> = Arc::new(store_or_skip!("contract"));

    assert!(store.list().await.expect("lists").is_empty());
    assert!(store.get("missing").await.expect("gets").is_none());
    assert!(!store.remove("missing").await.expect("removes"));

    assert!(!store.put("kitchen", graph("kitchen")).await.expect("stores"), "newly created");
    assert!(store.put("kitchen", graph("kitchen")).await.expect("stores"), "replaced");

    assert_eq!(store.get("kitchen").await.expect("gets"), Some(graph("kitchen")));

    store.put("bedroom", graph("bedroom")).await.expect("stores");
    assert_eq!(store.list().await.expect("lists"), ["bedroom", "kitchen"], "sorted");

    assert!(store.remove("kitchen").await.expect("removes"));
    assert_eq!(store.list().await.expect("lists"), ["bedroom"]);

    assert!(store.put("../escape", graph("escape")).await.is_err());
}

#[tokio::test]
async fn migrations_are_idempotent() {
    // Every replica runs them at startup, so the second one must be a no-op
    // rather than a crash loop.
    let first = store_or_skip!("idempotent");
    let url = scoped_url(&base_url().expect("a database"), "idempotent");
    let second = PostgresStore::connect(&url).await.expect("migrates twice");

    first.put("kitchen", graph("kitchen")).await.expect("stores");
    assert_eq!(second.get("kitchen").await.expect("gets"), Some(graph("kitchen")));
}

#[tokio::test]
async fn two_replicas_share_one_set_of_pipelines() {
    // This is the whole reason the backend exists.
    let writer = store_or_skip!("replicas");
    let url = scoped_url(&base_url().expect("a database"), "replicas");
    let reader = PostgresStore::connect(&url).await.expect("connects");

    writer.put("shared", graph("shared")).await.expect("stores");
    assert_eq!(reader.get("shared").await.expect("gets"), Some(graph("shared")));

    writer.remove("shared").await.expect("removes");
    assert!(reader.get("shared").await.expect("gets").is_none());
}

#[tokio::test]
async fn a_concurrent_write_does_not_lose_an_update() {
    // Read-then-write would let one replica's insert vanish; a single upsert
    // cannot. Whoever wins, the row must exist exactly once and be readable.
    let store = store_or_skip!("contention");
    let url = scoped_url(&base_url().expect("a database"), "contention");

    let writers: Vec<_> = (0..8)
        .map(|index| {
            let url = url.clone();
            tokio::spawn(async move {
                let store = PostgresStore::connect(&url).await.expect("connects");
                store.put("contended", graph(&format!("writer-{index}"))).await
            })
        })
        .collect();

    let mut created = 0;
    for writer in writers {
        if !writer.await.expect("the writer finished").expect("stores") {
            created += 1;
        }
    }

    assert_eq!(created, 1, "exactly one writer may report having created the row");
    assert_eq!(store.list().await.expect("lists"), ["contended"]);
    assert!(store.get("contended").await.expect("gets").is_some());
}

#[tokio::test]
async fn a_row_that_will_not_decode_is_reported_rather_than_hidden() {
    let store = store_or_skip!("undecodable");
    sqlx::query("INSERT INTO pipelines (name, graph) VALUES ($1, $2)")
        .bind("broken")
        .bind(serde_json::json!({ "nodes": "not a list" }))
        .execute(store.pool())
        .await
        .expect("inserts");

    let error = store.get("broken").await.expect_err("undecodable");
    assert!(error.to_string().contains("broken"), "{error}");
    assert_eq!(store.list().await.expect("lists"), ["broken"], "it is still listed");
}

#[tokio::test]
async fn the_graph_is_stored_as_queryable_json() {
    // jsonb rather than a blob, so operators can answer questions about
    // pipelines without deserializing them in application code.
    let store = store_or_skip!("queryable");
    store.put("kitchen", graph("kitchen")).await.expect("stores");

    let row: (String,) =
        sqlx::query_as("SELECT graph->>'name' FROM pipelines WHERE name = 'kitchen'")
            .fetch_one(store.pool())
            .await
            .expect("queries inside the document");
    assert_eq!(row.0, "kitchen");
}

#[tokio::test]
async fn an_unreachable_database_fails_with_an_actionable_message() {
    // No server needed: nothing is listening on this port.
    let error = PostgresStore::connect("postgres://127.0.0.1:1/nothing")
        .await
        .expect_err("cannot connect");
    assert!(error.to_string().contains("database"), "{error}");
}
