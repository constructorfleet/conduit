//! The contract every [`PipelineStore`] owes its callers.
//!
//! Swapping backends is a deployment decision, so it must not be a behaviour
//! decision. This module is the single definition of what "behaves like a
//! store" means; every backend's test binary includes it and runs it. A copy
//! per backend is how the backends drifted apart in the first place — a gap
//! only one of them had went unnoticed because only one of them was asked.

use std::sync::Arc;

use conduit_core::graph::PipelineGraph;
use conduit_core::id::SpeakerId;
use conduit_core::testing::voice_graph;
use conduit_provider::storage::{
    validate_name, EnrolledSpeaker, LlmVariant, McpTransport, PipelineStore,
    ProviderDefinition, ProviderDefinitionStore, ProviderDefinitionVariant, ProviderSecret,
    SpeakerRosterStore, ToolVariant,
};

/// A small but complete pipeline, named `name`.
pub fn graph(name: &str) -> PipelineGraph {
    voice_graph(name).stt("whisper").core("ollama").tts("piper").build()
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

/// The behaviour every Provider Definition Store backend owes its callers.
///
/// `store` must be empty. It is left non-empty.
pub async fn provider_definitions_behave_like_a_store(store: Arc<dyn ProviderDefinitionStore>) {
    an_empty_provider_definition_store_is_empty(&store).await;
    a_stored_provider_definition_comes_back(&store).await;
    provider_definition_ids_are_listed_in_order(&store).await;
    provider_definition_removal_is_visible(&store).await;
    every_provider_definition_method_refuses_an_unusable_id(&store).await;
    a_mismatched_provider_definition_id_is_refused(&store).await;
}

/// The behaviour every Speaker Roster Store backend owes its callers.
///
/// `store` must be empty. It is left non-empty.
pub async fn a_roster_behaves_like_a_store(store: Arc<dyn SpeakerRosterStore>) {
    an_empty_roster_is_empty(&store).await;
    a_stored_speaker_comes_back(&store).await;
    speaker_ids_are_listed_in_order(&store).await;
    speaker_removal_is_visible(&store).await;
    every_roster_method_refuses_an_unusable_id(&store).await;
    a_mismatched_speaker_id_is_refused(&store).await;
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

pub fn provider_definition(id: &str) -> ProviderDefinition {
    ProviderDefinition {
        id: id.to_owned(),
        label: format!("{id} Provider"),
        settings: Default::default(),
        variant: ProviderDefinitionVariant::Llm {
            variant: LlmVariant::OpenAi {
                base_url: "https://api.openai.example/v1".to_owned(),
                api_key: Some(ProviderSecret::Inline { value: "sk-test".to_owned() }),
                models: vec!["gpt-5".to_owned()],
                streaming: true,
                system_prompt: Some("Be terse.".to_owned()),
            },
        },
    }
}

fn replacement_provider_definition(id: &str) -> ProviderDefinition {
    // Carries a default setting so the round-trip below proves a backend keeps
    // stored settings across a restart, not just the connection variant.
    let mut settings = serde_json::Map::new();
    settings.insert("temperature".to_owned(), serde_json::json!(0.4));
    ProviderDefinition {
        id: id.to_owned(),
        label: format!("{id} Tools"),
        settings,
        variant: ProviderDefinitionVariant::Tool {
            variant: ToolVariant::Mcp {
                transport: McpTransport::StreamableHttp {
                    url: "https://tools.example.test/mcp".to_owned(),
                },
            },
        },
    }
}

async fn an_empty_provider_definition_store_is_empty(store: &Arc<dyn ProviderDefinitionStore>) {
    assert!(
        store.list().await.expect("lists").is_empty(),
        "a new provider definition store holds nothing"
    );
    assert!(store.get("missing").await.expect("gets").is_none());
    assert!(
        !store.remove("missing").await.expect("removes"),
        "removing nothing is not an error"
    );
}

async fn a_stored_provider_definition_comes_back(store: &Arc<dyn ProviderDefinitionStore>) {
    assert!(
        !store.put("openai", provider_definition("openai")).await.expect("stores"),
        "newly created"
    );
    assert!(
        store.put("openai", replacement_provider_definition("openai")).await.expect("stores"),
        "replaced"
    );

    let stored = store.get("openai").await.expect("gets").expect("present");
    assert_eq!(
        stored,
        replacement_provider_definition("openai"),
        "what went in comes back out"
    );
}

async fn provider_definition_ids_are_listed_in_order(store: &Arc<dyn ProviderDefinitionStore>) {
    store.put("bedroom", provider_definition("bedroom")).await.expect("stores");

    let ids = store.list().await.expect("lists");
    assert_eq!(ids, ["bedroom", "openai"], "sorted");
    for id in &ids {
        validate_name(id)
            .unwrap_or_else(|error| panic!("`{id}` was listed but is unusable: {error}"));
    }
}

async fn provider_definition_removal_is_visible(store: &Arc<dyn ProviderDefinitionStore>) {
    assert!(store.remove("openai").await.expect("removes"), "it was there");
    assert!(store.get("openai").await.expect("gets").is_none(), "and now it is not");
    assert_eq!(store.list().await.expect("lists"), ["bedroom"]);
    assert!(
        !store.remove("openai").await.expect("removes"),
        "removing it twice is not an error"
    );
}

async fn every_provider_definition_method_refuses_an_unusable_id(
    store: &Arc<dyn ProviderDefinitionStore>,
) {
    let before = store.list().await.expect("lists");

    for id in UNUSABLE_NAMES {
        assert!(
            store.put(id, provider_definition("escape")).await.is_err(),
            "put({id:?}) must be refused"
        );
        assert!(store.get(id).await.is_err(), "get({id:?}) must be refused");
        assert!(store.remove(id).await.is_err(), "remove({id:?}) must be refused");
    }

    assert_eq!(store.list().await.expect("lists"), before, "a refused id stores nothing");
}

async fn a_mismatched_provider_definition_id_is_refused(
    store: &Arc<dyn ProviderDefinitionStore>,
) {
    let before = store.get("bedroom").await.expect("gets");
    assert!(
        store.put("bedroom", provider_definition("other")).await.is_err(),
        "the route/store id and embedded definition id must match"
    );
    assert_eq!(
        store.get("bedroom").await.expect("gets"),
        before,
        "a refused replacement leaves the existing definition intact"
    );
}

/// A roster entry with a fixed id, so a test can name it twice.
pub fn speaker(id: SpeakerId, name: &str) -> EnrolledSpeaker {
    EnrolledSpeaker { id, ..EnrolledSpeaker::named(name) }
}

/// Two ids that sort in a known order, so listing can be asserted on.
///
/// Fixed rather than generated: a roster is keyed by UUID, and a test that
/// generated them would assert on whichever order the draw happened to give.
fn sample_ids() -> (SpeakerId, SpeakerId) {
    let first = SpeakerId::from_uuid(
        uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").expect("a uuid"),
    );
    let second = SpeakerId::from_uuid(
        uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").expect("a uuid"),
    );
    (first, second)
}

async fn an_empty_roster_is_empty(store: &Arc<dyn SpeakerRosterStore>) {
    assert!(store.list().await.expect("lists").is_empty(), "a new roster holds nobody");
    assert!(store.get("missing").await.expect("gets").is_none());
    assert!(
        !store.remove("missing").await.expect("removes"),
        "removing nothing is not an error"
    );
}

async fn a_stored_speaker_comes_back(store: &Arc<dyn SpeakerRosterStore>) {
    let (id, _) = sample_ids();
    let key = id.to_string();

    assert!(!store.put(&key, speaker(id, "Ada")).await.expect("stores"), "newly created");

    // Enrollment is what a replacement usually carries: the same person, now
    // with samples behind them.
    let enrolled = EnrolledSpeaker {
        samples: 2,
        provider: Some("voices".to_owned()),
        ..speaker(id, "Ada Lovelace")
    };
    assert!(store.put(&key, enrolled.clone()).await.expect("stores"), "replaced");

    let stored = store.get(&key).await.expect("gets").expect("present");
    assert_eq!(stored, enrolled, "what went in comes back out");
    assert!(stored.is_enrolled(), "and it still knows it has been heard");
}

async fn speaker_ids_are_listed_in_order(store: &Arc<dyn SpeakerRosterStore>) {
    let (first, second) = sample_ids();
    store.put(&second.to_string(), speaker(second, "Grace")).await.expect("stores");

    let ids = store.list().await.expect("lists");
    assert_eq!(ids, [first.to_string(), second.to_string()], "sorted");
    for id in &ids {
        validate_name(id)
            .unwrap_or_else(|error| panic!("`{id}` was listed but is unusable: {error}"));
    }
}

async fn speaker_removal_is_visible(store: &Arc<dyn SpeakerRosterStore>) {
    let (first, second) = sample_ids();
    let key = first.to_string();

    assert!(store.remove(&key).await.expect("removes"), "it was there");
    assert!(store.get(&key).await.expect("gets").is_none(), "and now it is not");
    assert_eq!(store.list().await.expect("lists"), [second.to_string()]);
    assert!(!store.remove(&key).await.expect("removes"), "removing it twice is not an error");
}

async fn every_roster_method_refuses_an_unusable_id(store: &Arc<dyn SpeakerRosterStore>) {
    let before = store.list().await.expect("lists");
    let (id, _) = sample_ids();

    for name in UNUSABLE_NAMES {
        assert!(store.put(name, speaker(id, "escape")).await.is_err(), "put({name:?})");
        assert!(store.get(name).await.is_err(), "get({name:?}) must be refused");
        assert!(store.remove(name).await.is_err(), "remove({name:?}) must be refused");
    }

    assert_eq!(store.list().await.expect("lists"), before, "a refused id stores nothing");
}

async fn a_mismatched_speaker_id_is_refused(store: &Arc<dyn SpeakerRosterStore>) {
    // A roster row whose key was not the id it carries would identify the
    // wrong person: a turn reports the id the service answered with, and that
    // is what the key is looked up by.
    let (first, second) = sample_ids();
    let key = second.to_string();
    let before = store.get(&key).await.expect("gets");

    assert!(
        store.put(&key, speaker(first, "somebody else")).await.is_err(),
        "the store key and the id the entry carries must match"
    );
    assert_eq!(
        store.get(&key).await.expect("gets"),
        before,
        "a refused replacement leaves the existing entry intact"
    );
}
