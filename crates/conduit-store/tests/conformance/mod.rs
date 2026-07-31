//! The contract every [`PipelineStore`] owes its callers.
//!
//! Swapping backends is a deployment decision, so it must not be a behaviour
//! decision. This module is the single definition of what "behaves like a
//! store" means; every backend's test binary includes it and runs it. A copy
//! per backend is how the backends drifted apart in the first place — a gap
//! only one of them had went unnoticed because only one of them was asked.

use std::sync::Arc;

use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_provider::storage::{validate_name, PipelineStore};

/// A small but complete pipeline, named `name`.
pub fn graph(name: &str) -> PipelineGraph {
    PipelineGraph::new(name)
        .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
        .with_node(Node::new("llm", NodeKind::Llm, "ollama"))
        .with_node(Node::new("tts", NodeKind::Tts, "piper"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

/// Names no backend may accept, whatever it would do with them.
///
/// A name arrives from a URL path and becomes a file name or a row key, so the
/// interesting cases are the ones that mean something to a filesystem.
pub const UNUSABLE_NAMES: [&str; 7] =
    ["../escape", "..", ".", "a/b", "a\\b", "kitchen light", ""];

/// The behaviour every backend owes its callers.
///
/// `store` must be empty. It is left non-empty.
pub async fn behaves_like_a_store(store: Arc<dyn PipelineStore>) {
    an_empty_store_is_empty(&store).await;
    a_stored_pipeline_comes_back(&store).await;
    names_are_listed_in_order(&store).await;
    removal_is_visible(&store).await;
    every_method_refuses_an_unusable_name(&store).await;
}

/// Nothing is present, and asking for nothing is not an error.
async fn an_empty_store_is_empty(store: &Arc<dyn PipelineStore>) {
    assert!(store.list().await.expect("lists").is_empty(), "a new store holds nothing");
    assert!(store.get("missing").await.expect("gets").is_none());
    assert!(
        !store.remove("missing").await.expect("removes"),
        "removing nothing is not an error"
    );
}

/// A pipeline round-trips unchanged, and `put` reports whether it replaced one.
async fn a_stored_pipeline_comes_back(store: &Arc<dyn PipelineStore>) {
    assert!(!store.put("kitchen", graph("kitchen")).await.expect("stores"), "newly created");
    assert!(store.put("kitchen", graph("kitchen")).await.expect("stores"), "replaced");

    let stored = store.get("kitchen").await.expect("gets").expect("present");
    assert_eq!(stored, graph("kitchen"), "what went in comes back out");
}

/// `list` is sorted, and only ever names that could have been stored.
async fn names_are_listed_in_order(store: &Arc<dyn PipelineStore>) {
    store.put("bedroom", graph("bedroom")).await.expect("stores");

    let names = store.list().await.expect("lists");
    assert_eq!(names, ["bedroom", "kitchen"], "sorted");
    for name in &names {
        validate_name(name)
            .unwrap_or_else(|error| panic!("`{name}` was listed but is unusable: {error}"));
    }
}

/// What is removed is gone, from both `get` and `list`.
async fn removal_is_visible(store: &Arc<dyn PipelineStore>) {
    assert!(store.remove("kitchen").await.expect("removes"), "it was there");
    assert!(store.get("kitchen").await.expect("gets").is_none(), "and now it is not");
    assert_eq!(store.list().await.expect("lists"), ["bedroom"]);
    assert!(
        !store.remove("kitchen").await.expect("removes"),
        "removing it twice is not an error"
    );
}

/// Every method rejects a name the store could not have created.
///
/// `get` and `remove` fail rather than reporting absence: an unusable name is
/// a mistake in the request, and answering "there is no such pipeline" invites
/// the caller to try creating it under the same name — which cannot work.
async fn every_method_refuses_an_unusable_name(store: &Arc<dyn PipelineStore>) {
    let before = store.list().await.expect("lists");

    for name in UNUSABLE_NAMES {
        assert!(
            store.put(name, graph("escape")).await.is_err(),
            "put({name:?}) must be refused"
        );
        assert!(store.get(name).await.is_err(), "get({name:?}) must be refused");
        assert!(store.remove(name).await.is_err(), "remove({name:?}) must be refused");
    }

    assert_eq!(store.list().await.expect("lists"), before, "a refused name stores nothing");
}
