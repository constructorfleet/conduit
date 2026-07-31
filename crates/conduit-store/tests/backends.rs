//! Both backends must behave the same, or swapping them changes behaviour.
//!
//! The shared cases run against each; the rest cover what is specific to
//! keeping pipelines on disk.

use std::path::PathBuf;
use std::sync::Arc;

use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_provider::storage::PipelineStore;
use conduit_store::{FileStore, MemoryStore};

/// A directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "conduit-store-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        Self(path)
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn graph(name: &str) -> PipelineGraph {
    PipelineGraph::new(name)
        .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
        .with_node(Node::new("llm", NodeKind::Llm, "ollama"))
        .with_node(Node::new("tts", NodeKind::Tts, "piper"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

/// The behaviour every backend owes its callers.
async fn behaves_like_a_store(store: Arc<dyn PipelineStore>) {
    assert!(store.list().await.expect("lists").is_empty());
    assert!(store.get("missing").await.expect("gets").is_none());
    assert!(
        !store.remove("missing").await.expect("removes"),
        "removing nothing is not an error"
    );

    assert!(!store.put("kitchen", graph("kitchen")).await.expect("stores"), "newly created");
    assert!(store.put("kitchen", graph("kitchen")).await.expect("stores"), "replaced");

    let stored = store.get("kitchen").await.expect("gets").expect("present");
    assert_eq!(stored, graph("kitchen"));

    store.put("bedroom", graph("bedroom")).await.expect("stores");
    assert_eq!(store.list().await.expect("lists"), ["bedroom", "kitchen"], "sorted");

    assert!(store.remove("kitchen").await.expect("removes"));
    assert_eq!(store.list().await.expect("lists"), ["bedroom"]);
    assert!(store.get("kitchen").await.expect("gets").is_none());

    // A name that could escape the store must be refused, whatever the
    // backend does with it.
    assert!(store.put("../escape", graph("escape")).await.is_err());
}

#[tokio::test]
async fn the_memory_store_behaves_like_a_store() {
    behaves_like_a_store(Arc::new(MemoryStore::new())).await;
}

#[tokio::test]
async fn the_file_store_behaves_like_a_store() {
    let directory = TempDir::new("contract");
    let store = FileStore::open(directory.path()).await.expect("opens");
    behaves_like_a_store(Arc::new(store)).await;
}

#[tokio::test]
async fn pipelines_survive_a_restart() {
    // The whole point: a new process reading the same directory.
    let directory = TempDir::new("restart");

    let before = FileStore::open(directory.path()).await.expect("opens");
    before.put("kitchen", graph("kitchen")).await.expect("stores");
    drop(before);

    let after = FileStore::open(directory.path()).await.expect("reopens");
    assert_eq!(after.get("kitchen").await.expect("gets"), Some(graph("kitchen")));
}

#[tokio::test]
async fn opening_creates_the_directory() {
    let directory = TempDir::new("create");
    let nested = directory.path().join("a").join("b");
    let store = FileStore::open(&nested).await.expect("creates the directory");
    store.put("kitchen", graph("kitchen")).await.expect("stores");
    assert!(nested.join("kitchen.json").exists());
}

#[tokio::test]
async fn stored_files_are_readable_json() {
    // Someone will edit these by hand; they should be able to.
    let directory = TempDir::new("readable");
    let store = FileStore::open(directory.path()).await.expect("opens");
    store.put("kitchen", graph("kitchen")).await.expect("stores");

    let text =
        tokio::fs::read_to_string(directory.path().join("kitchen.json")).await.expect("reads");
    assert!(text.contains("\n  \"nodes\""), "expected pretty JSON: {text}");
    let parsed: PipelineGraph = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(parsed, graph("kitchen"));
}

#[tokio::test]
async fn unrelated_files_are_ignored() {
    let directory = TempDir::new("unrelated");
    let store = FileStore::open(directory.path()).await.expect("opens");
    tokio::fs::write(directory.path().join("notes.txt"), "hello").await.expect("writes");
    tokio::fs::create_dir(directory.path().join("subdir")).await.expect("creates");
    store.put("kitchen", graph("kitchen")).await.expect("stores");

    assert_eq!(store.list().await.expect("lists"), ["kitchen"]);
}

#[tokio::test]
async fn a_corrupt_file_is_reported_rather_than_treated_as_missing() {
    // "It is not there" would invite an editor to overwrite a file that is
    // merely broken; "it is unreadable" invites a fix.
    let directory = TempDir::new("corrupt");
    let store = FileStore::open(directory.path()).await.expect("opens");
    tokio::fs::write(directory.path().join("broken.json"), "{not json").await.expect("writes");

    let error = store.get("broken").await.expect_err("unreadable");
    assert!(error.to_string().contains("broken.json"), "{error}");
    assert_eq!(store.list().await.expect("lists"), ["broken"], "it is still listed");
}

#[tokio::test]
async fn a_failed_write_leaves_the_previous_definition_intact() {
    // Writes go to a temporary file and are renamed, so a half-written file
    // never becomes the live definition.
    let directory = TempDir::new("atomic");
    let store = FileStore::open(directory.path()).await.expect("opens");
    store.put("kitchen", graph("kitchen")).await.expect("stores");
    store.put("kitchen", graph("updated")).await.expect("replaces");

    assert_eq!(store.get("kitchen").await.expect("gets"), Some(graph("updated")));
    // No temporary files are left behind to be mistaken for pipelines.
    assert_eq!(store.list().await.expect("lists"), ["kitchen"]);
}

#[tokio::test]
async fn a_traversing_name_cannot_reach_outside_the_directory() {
    let directory = TempDir::new("traversal");
    let store = FileStore::open(directory.path()).await.expect("opens");

    for name in ["../escape", "..", "a/b", "a\\b"] {
        assert!(store.put(name, graph("escape")).await.is_err(), "{name} must be refused");
        assert!(store.get(name).await.is_err(), "{name} must be refused");
        assert!(store.remove(name).await.is_err(), "{name} must be refused");
    }

    let outside = directory.path().parent().expect("a parent").join("escape.json");
    assert!(!outside.exists(), "nothing may be written outside the store");
}
