//! The in-process store, end to end.
//!
//! Everything here runs with no external service, which is the point of the
//! backend: these tests never skip.

use std::path::PathBuf;

use conduit_core::id::{ConversationId, SpeakerId};
use conduit_core::memory::Scope;
use conduit_memory::Builtin;
use conduit_provider::memory::{Match, Memory, Query, Record};
use conduit_provider::{Health, Provider};

/// A directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "conduit-memory-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("creates the directory");
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

/// An ephemeral store, which is the shape a deployment gets by default.
async fn ephemeral() -> Builtin {
    Builtin::builder("recall").build().await.expect("builds")
}

#[tokio::test]
async fn a_relevant_record_is_recalled_ahead_of_an_irrelevant_one() {
    let memory = ephemeral().await;
    memory.store(global("the cat is called mabel")).await.expect("stores");
    memory.store(global("the recycling goes out on tuesday")).await.expect("stores");

    let found =
        memory.search(Query::new("when does the recycling go out", 5)).await.expect("searches");

    assert_eq!(
        contents(&found).first(),
        Some(&"the recycling goes out on tuesday"),
        "{found:?}"
    );
}

#[tokio::test]
async fn a_record_sharing_no_word_with_the_question_is_not_recalled_at_all() {
    // Returning it with a score of zero would dress a record that did not
    // match up as one that did, and the runtime discards the score.
    let memory = ephemeral().await;
    memory.store(global("the cat is called mabel")).await.expect("stores");

    let found = memory.search(Query::new("recycling", 5)).await.expect("searches");

    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn an_empty_store_reports_finding_nothing_rather_than_failing() {
    // The first turn of every conversation lands here, and an error would emit
    // a warning every single time.
    let memory = ephemeral().await;

    let found = memory.search(Query::new("anything at all", 5)).await.expect("searches");

    assert!(found.is_empty());
}

#[tokio::test]
async fn every_score_lands_between_zero_and_one() {
    let memory = ephemeral().await;
    for content in [
        "recycling day is tuesday",
        "recycling is collected fortnightly on this street",
        "the bins outside are green",
    ] {
        memory.store(global(content)).await.expect("stores");
    }

    let found = memory.search(Query::new("recycling collected", 5)).await.expect("searches");

    assert!(!found.is_empty());
    for item in &found {
        assert!((0.0..=1.0).contains(&item.score), "{item:?}");
    }
}

#[tokio::test]
async fn the_scores_come_back_in_descending_order() {
    let memory = ephemeral().await;
    memory.store(global("recycling")).await.expect("stores");
    memory.store(global("recycling day is tuesday")).await.expect("stores");

    let found = memory.search(Query::new("recycling day tuesday", 5)).await.expect("searches");

    assert!(found.len() >= 2, "{found:?}");
    for pair in found.windows(2) {
        assert!(pair[0].score >= pair[1].score, "{found:?}");
    }
}

#[tokio::test]
async fn a_query_naming_a_scope_does_not_recall_records_of_another() {
    let memory = ephemeral().await;
    memory.store(global("recycling is global")).await.expect("stores");
    memory
        .store(Record { scope: Scope::Conversation, ..global("recycling is per conversation") })
        .await
        .expect("stores");

    let found = memory
        .search(Query { scope: Some(Scope::Global), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    assert_eq!(contents(&found), ["recycling is global"]);
}

#[tokio::test]
async fn a_query_naming_a_conversation_does_not_recall_another_conversation() {
    let mine = ConversationId::new();
    let theirs = ConversationId::new();
    let memory = ephemeral().await;
    memory
        .store(Record { conversation: Some(mine), ..global("recycling in mine") })
        .await
        .expect("stores");
    memory
        .store(Record { conversation: Some(theirs), ..global("recycling in theirs") })
        .await
        .expect("stores");

    let found = memory
        .search(Query { conversation: Some(mine), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    assert_eq!(contents(&found), ["recycling in mine"]);
}

#[tokio::test]
async fn a_record_belonging_to_no_conversation_is_recalled_by_any_conversation() {
    // A global fact is not conversation-specific, so restricting a search to
    // one conversation must not hide it.
    let memory = ephemeral().await;
    memory.store(global("the recycling goes out on tuesday")).await.expect("stores");

    let found = memory
        .search(Query {
            conversation: Some(ConversationId::new()),
            ..Query::new("recycling", 5)
        })
        .await
        .expect("searches");

    assert_eq!(contents(&found), ["the recycling goes out on tuesday"]);
}

#[tokio::test]
async fn a_query_naming_a_speaker_does_not_recall_another_speakers_record() {
    let mine = SpeakerId::new();
    let memory = ephemeral().await;
    memory
        .store(Record {
            scope: Scope::Speaker,
            speaker: Some(mine),
            ..global("recycling is mine")
        })
        .await
        .expect("stores");
    memory
        .store(Record {
            scope: Scope::Speaker,
            speaker: Some(SpeakerId::new()),
            ..global("recycling is theirs")
        })
        .await
        .expect("stores");

    let found = memory
        .search(Query { speaker: Some(mine), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    assert_eq!(contents(&found), ["recycling is mine"]);
}

#[tokio::test]
async fn a_speaker_scoped_record_with_no_speaker_is_recalled_by_every_speaker() {
    // The documented privacy-shaped default. The runtime has no speaker
    // identification today, so this is what a `scope: speaker` binding
    // actually produces — and refusing the write instead would discard every
    // turn with no signal at all.
    let memory = ephemeral().await;
    memory
        .store(Record { scope: Scope::Speaker, ..global("the recycling goes out tuesday") })
        .await
        .expect("stores");

    for _ in 0..2 {
        let found = memory
            .search(Query { speaker: Some(SpeakerId::new()), ..Query::new("recycling", 5) })
            .await
            .expect("searches");
        assert_eq!(contents(&found), ["the recycling goes out tuesday"]);
    }
}

#[tokio::test]
async fn a_record_the_query_excludes_does_not_change_how_the_rest_rank() {
    // Filtering happens before scoring, so an excluded record contributes
    // nothing to the document frequencies. Were it the other way round, the
    // scores of a per-conversation search would depend on other conversations.
    let mine = ConversationId::new();
    let memory = ephemeral().await;
    memory
        .store(Record { conversation: Some(mine), ..global("recycling day is tuesday") })
        .await
        .expect("stores");

    let alone = memory
        .search(Query { conversation: Some(mine), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    for _ in 0..5 {
        memory
            .store(Record {
                conversation: Some(ConversationId::new()),
                ..global("recycling elsewhere")
            })
            .await
            .expect("stores");
    }

    let crowded = memory
        .search(Query { conversation: Some(mine), ..Query::new("recycling", 5) })
        .await
        .expect("searches");

    assert_eq!(contents(&alone), contents(&crowded));
}

#[tokio::test]
async fn the_limit_bounds_how_much_comes_back() {
    let memory = ephemeral().await;
    for index in 0..10 {
        memory.store(global(&format!("recycling note {index}"))).await.expect("stores");
    }

    let found = memory.search(Query::new("recycling", 3)).await.expect("searches");

    assert_eq!(found.len(), 3);
}

#[tokio::test]
async fn the_oldest_record_is_dropped_once_the_capacity_is_reached() {
    let memory = Builtin::builder("recall").capacity(2).build().await.expect("builds");
    memory.store(global("recycling note one")).await.expect("stores");
    memory.store(global("recycling note two")).await.expect("stores");
    memory.store(global("recycling note three")).await.expect("stores");

    let found = memory.search(Query::new("recycling note", 10)).await.expect("searches");

    let recalled = contents(&found);
    assert_eq!(recalled.len(), 2, "{found:?}");
    assert!(!recalled.contains(&"recycling note one"), "the oldest went first: {recalled:?}");
}

#[tokio::test]
async fn forgetting_a_conversation_leaves_every_other_record_alone() {
    let doomed = ConversationId::new();
    let kept = ConversationId::new();
    let memory = ephemeral().await;
    memory
        .store(Record { conversation: Some(doomed), ..global("recycling in the doomed one") })
        .await
        .expect("stores");
    memory
        .store(Record { conversation: Some(kept), ..global("recycling in the kept one") })
        .await
        .expect("stores");
    memory.store(global("recycling everywhere")).await.expect("stores");

    memory.forget_conversation(doomed).await.expect("forgets");

    let found = memory.search(Query::new("recycling", 10)).await.expect("searches");
    let recalled = contents(&found);
    assert!(!recalled.contains(&"recycling in the doomed one"), "{recalled:?}");
    assert!(recalled.contains(&"recycling in the kept one"), "{recalled:?}");
    assert!(recalled.contains(&"recycling everywhere"), "{recalled:?}");
}

#[tokio::test]
async fn forgetting_a_conversation_nothing_was_stored_for_succeeds() {
    // Documented as having to succeed even when nothing was stored: it is
    // called when a conversation ends, whether or not it remembered anything.
    let memory = ephemeral().await;

    memory.forget_conversation(ConversationId::new()).await.expect("forgets nothing");
}

#[tokio::test]
async fn records_survive_being_reconstructed_from_the_same_file() {
    let directory = TempDir::new("persist");
    let path = directory.file("memory.json");

    let first = Builtin::builder("recall").path(&path).build().await.expect("builds");
    first.store(global("the recycling goes out on tuesday")).await.expect("stores");
    drop(first);

    let second = Builtin::builder("recall").path(&path).build().await.expect("rebuilds");
    let found = second.search(Query::new("recycling", 5)).await.expect("searches");

    assert_eq!(contents(&found), ["the recycling goes out on tuesday"]);
}

#[tokio::test]
async fn forgetting_a_conversation_survives_being_reconstructed() {
    // Otherwise a restart would resurrect exactly the records a person asked
    // to have deleted, which is the one failure mode of this that matters.
    let directory = TempDir::new("forget-persist");
    let path = directory.file("memory.json");
    let doomed = ConversationId::new();

    let first = Builtin::builder("recall").path(&path).build().await.expect("builds");
    first
        .store(Record { conversation: Some(doomed), ..global("recycling in the doomed one") })
        .await
        .expect("stores");
    first.forget_conversation(doomed).await.expect("forgets");
    drop(first);

    let second = Builtin::builder("recall").path(&path).build().await.expect("rebuilds");
    let found = second.search(Query::new("recycling", 5)).await.expect("searches");

    assert!(found.is_empty(), "{found:?}");
}

#[tokio::test]
async fn a_store_with_no_path_remembers_nothing_after_it_is_dropped() {
    // The default is genuinely ephemeral, not "a file somewhere you did not
    // ask for".
    let first = ephemeral().await;
    first.store(global("the recycling goes out on tuesday")).await.expect("stores");
    drop(first);

    let second = ephemeral().await;
    let found = second.search(Query::new("recycling", 5)).await.expect("searches");

    assert!(found.is_empty());
}

#[tokio::test]
async fn a_file_that_will_not_parse_is_a_configuration_error_at_build_time() {
    // Starting with an empty store instead would be silent data loss, and the
    // operator who mistyped a path would never hear about it.
    let directory = TempDir::new("corrupt");
    let path = directory.file("memory.json");
    tokio::fs::write(&path, "{not json").await.expect("writes");

    let error = Builtin::builder("recall").path(&path).build().await.expect_err("unparseable");

    assert!(error.to_string().contains("memory.json"), "{error}");
    assert!(matches!(error, conduit_core::Error::Config(_)), "{error:?}");
}

#[tokio::test]
async fn a_file_from_a_future_format_is_refused_rather_than_misread() {
    let directory = TempDir::new("future");
    let path = directory.file("memory.json");
    tokio::fs::write(&path, br#"{"version":99,"records":[]}"#).await.expect("writes");

    let error = Builtin::builder("recall").path(&path).build().await.expect_err("too new");

    assert!(error.to_string().contains("version"), "{error}");
}

#[tokio::test]
async fn a_file_that_is_not_there_yet_is_an_empty_store_rather_than_an_error() {
    // First run of a deployment that has just configured a path.
    let directory = TempDir::new("absent");
    let path = directory.file("memory.json");

    let memory = Builtin::builder("recall").path(&path).build().await.expect("builds");

    assert!(memory.search(Query::new("recycling", 5)).await.expect("searches").is_empty());
}

#[tokio::test]
async fn a_half_written_file_never_replaces_the_previous_one() {
    // Writes land beside the target and are renamed, so the live file is
    // either the old set or the new one and never a truncation of either.
    let directory = TempDir::new("atomic");
    let path = directory.file("memory.json");

    let memory = Builtin::builder("recall").path(&path).build().await.expect("builds");
    memory.store(global("recycling note one")).await.expect("stores");
    memory.store(global("recycling note two")).await.expect("stores");

    let live = tokio::fs::read(&path).await.expect("reads");
    serde_json::from_slice::<serde_json::Value>(&live).expect("the live file is whole");
    assert!(
        !tokio::fs::try_exists(path.with_extension("json.tmp")).await.unwrap_or(false),
        "the temporary file is renamed away, not left behind"
    );
}

#[tokio::test]
async fn a_store_that_persists_nowhere_is_healthy_because_there_is_nothing_to_reach() {
    let memory = ephemeral().await;

    assert_eq!(memory.health().await, Health::Healthy);
}

#[tokio::test]
async fn a_store_whose_file_cannot_be_written_is_degraded_rather_than_unhealthy() {
    // It still stores and still recalls for as long as this process lives,
    // which is most of what this store does. Unhealthy would take it out of
    // service over the half that is broken.
    let directory = TempDir::new("unwritable");
    // A path whose parent is a file, so no directory can be created for it and
    // no file can be written into it. Portable in a way that chmod is not.
    let blocker = directory.file("blocker");
    tokio::fs::write(&blocker, b"not a directory").await.expect("writes");
    let path = blocker.join("memory.json");

    let memory = Builtin::builder("recall").path(&path).build().await.expect("builds");

    match memory.health().await {
        Health::Degraded { reason } => {
            assert!(reason.contains("memory.json"), "{reason}");
            assert!(reason.contains("recall works"), "{reason}");
        }
        other => panic!("expected a degraded store, got {other:?}"),
    }
    // And it is still usable, which is what "degraded" is claiming.
    assert!(memory.health().await.is_usable());
    assert!(memory.store(global("recycling")).await.is_err(), "the write itself fails");
    let found = memory.search(Query::new("recycling", 5)).await.expect("searches anyway");
    assert_eq!(contents(&found), ["recycling"], "the in-process half still works");
}

#[tokio::test]
async fn a_store_reports_the_identity_it_was_built_with() {
    let memory =
        Builtin::builder("recall").label("Recall (kitchen)").build().await.expect("builds");

    assert_eq!(Provider::name(&memory), "recall");
    assert_eq!(memory.descriptor().label, "Recall (kitchen)");
    assert_eq!(memory.descriptor().capability, conduit_provider::Capability::Memory);
}

#[tokio::test]
async fn the_default_capacity_is_the_documented_one() {
    let memory = ephemeral().await;

    assert_eq!(memory.capacity(), conduit_memory::builtin::DEFAULT_CAPACITY);
}
