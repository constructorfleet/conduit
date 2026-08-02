//! Behaviour of one conversation turn, driven through fake providers.
//!
//! The fakes record what they were asked to do, so these tests describe the
//! runtime's contract — what reaches each stage, what reaches the bus, and in
//! what order — without depending on any real model.

mod fakes;

use std::time::Duration;

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::bus::{EventBus, Subscription};
use conduit_core::event::{CancelReason, Event};
use conduit_core::graph::{
    Edge, MemoryBinding, MemoryMode, Modality, Node, PipelineGraph, ReasoningCore,
};
use conduit_core::testing::voice_graph;
use conduit_core::Error;
use conduit_provider::stt::Transcript;
use conduit_runtime::{Providers, Reply, Runner};
use fakes::{audio_of, FailingStt, FakeLlm, FakeStt, FakeTts, HangingTts, SlowTts};
use futures_util::StreamExt;

/// mic -> stt -> core -> tts, the shape the runtime can execute today.
fn linear_graph() -> PipelineGraph {
    voice_graph("test").source("test").stt("fake-stt").core("fake-llm").tts("fake-tts").build()
}

/// Reads events until the turn ends, so assertions see the whole sequence.
async fn drain(subscription: &mut Subscription) -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_secs(5), subscription.recv()).await
    {
        let terminal = envelope.event.is_terminal();
        events.push(envelope.event.clone());
        if terminal {
            break;
        }
    }
    events
}

/// Variant names, which is what the ordering assertions care about.
fn names(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            serde_json::to_value(event).expect("serialize")["type"]
                .as_str()
                .expect("tag")
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn speaks_the_model_response_for_a_typed_utterance() {
    // A pipeline fed by text has no recognizer at all, so the turn has to
    // reach the model without one rather than transcribing something.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers =
        Providers::new().with_llm(FakeLlm::new(vec!["Done."])).with_tts(FakeTts::new());
    let graph = PipelineGraph::new("typed")
        .with_node(Node::source("chat", "test", Modality::Text))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("chat", "core"))
        .with_edge(Edge::new("core", "tts"));

    let runner = Runner::prepare(&graph, &providers, bus).expect("graph is executable");
    let spoken: Vec<_> = runner.run_text("turn on the light").speech().collect().await;

    let audio: Vec<u8> = spoken
        .into_iter()
        .map(|chunk| chunk.expect("chunk"))
        .flat_map(|chunk| chunk.data.to_vec())
        .collect();
    assert_eq!(String::from_utf8(audio).expect("utf-8"), "Done.");

    // No capture and no partials, because nothing was heard — but SpeechFinal
    // still opens the turn, so a reconstruction shows what was asked without
    // needing to know which modality asked it.
    let events = drain(&mut subscription).await;
    let published = names(&events);
    assert!(!published.contains(&"AudioStarted".to_owned()), "{published:?}");
    assert!(!published.contains(&"SpeechPartial".to_owned()), "{published:?}");
    assert_eq!(
        published.iter().filter(|name| *name == "SpeechFinal").count(),
        1,
        "{published:?}"
    );
    let Some(Event::SpeechFinal { text, .. }) =
        events.iter().find(|event| matches!(event, Event::SpeechFinal { .. }))
    else {
        panic!("a typed turn still reports what was asked");
    };
    assert_eq!(text, "turn on the light");
}

#[tokio::test]
async fn a_text_pipeline_writes_its_reply_down_with_no_speech_providers() {
    // The whole point of the track: a working pipeline with neither a
    // recognizer nor a synthesizer configured.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new().with_llm(FakeLlm::new(vec!["One. ", "Two."]));
    let graph = PipelineGraph::new("chat")
        .with_node(Node::source("in", "test", Modality::Text))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::sink("out", "test", Modality::Text))
        .with_edge(Edge::new("in", "core"))
        .with_edge(Edge::new("core", "out"));

    let runner = Runner::prepare(&graph, &providers, bus).expect("graph is executable");
    let replies: Vec<_> = runner.run_text("hello").output.collect().await;

    let written: Vec<String> = replies
        .into_iter()
        .filter_map(|reply| match reply.expect("reply") {
            Reply::Text(text) => Some(text),
            Reply::Speech(_) => None,
        })
        .collect();
    // Sentence segmentation is what a voice pipeline speaks a piece at a time,
    // and it is what a text pipeline writes a piece at a time. Same boundary,
    // including the trimming — a written segment carries no more trailing
    // space than a spoken one carries trailing silence.
    assert_eq!(written, ["One.", "Two."]);

    // Nothing was synthesized, so nothing announces a voice.
    let published = names(&drain(&mut subscription).await);
    assert!(!published.contains(&"TtsStarted".to_owned()), "{published:?}");
    assert!(
        published.contains(&"UtteranceSegmentStarted".to_owned()),
        "a written segment is still a segment: {published:?}"
    );
}

/// A pipeline whose core is bound to one memory store.
fn remembering_graph(mode: MemoryMode) -> PipelineGraph {
    let core = ReasoningCore {
        memory: vec![MemoryBinding {
            provider: "fake-memory".to_owned(),
            mode,
            scope: None,
            limit: 5,
        }],
        ..ReasoningCore::new("fake-llm")
    };
    PipelineGraph::new("remembering")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "core".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "core"))
        .with_edge(Edge::new("core", "tts"))
}

#[tokio::test]
async fn a_core_is_reminded_before_it_answers_and_stores_after() {
    let memory = fakes::FakeMemory::recalling("the oven is in the kitchen");
    let llm = FakeLlm::new(vec!["It is in the kitchen."]);
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("where is the oven")]))
        .with_llm(llm.clone())
        .with_tts(FakeTts::new())
        .with_memory(memory.clone());

    let runner = Runner::prepare(
        &remembering_graph(MemoryMode::ReadWrite),
        &providers,
        EventBus::default(),
    )
    .expect("executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    let searched = memory.searched();
    assert_eq!(searched.len(), 1, "retrieved once, before the first model call");
    assert_eq!(searched[0].text, "where is the oven");
    assert_eq!(searched[0].limit, 5, "the binding's limit is what is asked for");

    // What was recalled has to actually reach the model, or retrieval is
    // theatre: the store was read and the answer was given without it.
    let prompt = llm.requests()[0]
        .messages
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("the oven is in the kitchen"), "{prompt}");

    let stored = memory.stored();
    assert_eq!(stored.len(), 1, "and the exchange is stored once, after the reply");
    assert!(stored[0].content.contains("where is the oven"), "{:?}", stored[0]);
    assert!(stored[0].content.contains("It is in the kitchen."), "{:?}", stored[0]);
}

#[tokio::test]
async fn a_read_only_binding_never_writes() {
    let memory = fakes::FakeMemory::recalling("something");
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new())
        .with_memory(memory.clone());

    let runner =
        Runner::prepare(&remembering_graph(MemoryMode::Read), &providers, EventBus::default())
            .expect("executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    assert_eq!(memory.searched().len(), 1);
    assert!(memory.stored().is_empty(), "a read binding must not write");
}

#[tokio::test]
async fn a_write_only_binding_never_reads() {
    let memory = fakes::FakeMemory::recalling("something the model must not see");
    let llm = FakeLlm::new(vec!["Hello."]);
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(llm.clone())
        .with_tts(FakeTts::new())
        .with_memory(memory.clone());

    let runner =
        Runner::prepare(&remembering_graph(MemoryMode::Write), &providers, EventBus::default())
            .expect("executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    assert!(memory.searched().is_empty(), "a write binding must not read");
    assert_eq!(memory.stored().len(), 1);
    let prompt = llm.requests()[0]
        .messages
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!prompt.contains("must not see"), "{prompt}");
}

/// Two ways in and two ways out, around one core.
fn hybrid_graph() -> PipelineGraph {
    PipelineGraph::new("hybrid")
        .with_node(Node::source("mic", "test", Modality::Audio))
        .with_node(Node::source("chat", "test", Modality::Text))
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_node(Node::sink("speaker", "test", Modality::Audio))
        .with_node(Node::sink("transcript", "test", Modality::Text))
        .with_edge(Edge::new("mic", "stt"))
        .with_edge(Edge::new("stt", "core"))
        .with_edge(Edge::new("chat", "core"))
        .with_edge(Edge::new("core", "tts"))
        .with_edge(Edge::new("tts", "speaker"))
        .with_edge(Edge::new("core", "transcript"))
}

#[tokio::test]
async fn a_hybrid_pipeline_both_speaks_and_writes_its_reply() {
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["One. ", "Two."]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&hybrid_graph(), &providers, EventBus::default()).expect("executable");
    let replies: Vec<_> = runner.run(audio_of(&["a"])).output.collect().await;

    let mut spoken = Vec::new();
    let mut written = Vec::new();
    for reply in replies {
        match reply.expect("reply") {
            Reply::Speech(chunk) => spoken.extend_from_slice(&chunk.data),
            Reply::Text(text) => written.push(text),
        }
    }

    assert_eq!(written, ["One.", "Two."], "the transcript sink gets the words");
    assert_eq!(
        String::from_utf8(spoken).expect("utf-8"),
        "One.Two.",
        "and the speaker gets the audio for the same sentences"
    );
}

#[tokio::test]
async fn a_hybrid_pipeline_takes_either_kind_of_input() {
    // One core, reached from a microphone or from a chat box. Which one a turn
    // used is the caller's choice, not the graph's.
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Done."]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&hybrid_graph(), &providers, EventBus::default()).expect("executable");
    let typed: Vec<_> = runner.run_text("hi").output.collect().await;

    assert!(
        typed.iter().any(|reply| matches!(reply, Ok(Reply::Text(_)))),
        "a typed question is answered by the same pipeline"
    );
}

#[tokio::test]
async fn audio_handed_to_a_text_pipeline_fails_the_turn_rather_than_panicking() {
    // The mismatch is a caller's, not an operator's, but it happens inside a
    // spawned task where a panic would be invisible.
    let bus = EventBus::default();
    let providers =
        Providers::new().with_llm(FakeLlm::new(vec!["Done."])).with_tts(FakeTts::new());
    let graph = PipelineGraph::new("typed")
        .with_node(Node::source("chat", "test", Modality::Text))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("chat", "core"))
        .with_edge(Edge::new("core", "tts"));

    let runner = Runner::prepare(&graph, &providers, bus).expect("graph is executable");
    let spoken: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    assert!(
        spoken.iter().any(std::result::Result::is_err),
        "the turn must report the mismatch rather than speaking anyway"
    );
}

#[tokio::test]
async fn speaks_the_model_response_for_a_captured_utterance() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![
            Transcript::partial("turn on"),
            Transcript::final_text("turn on the light"),
        ]))
        .with_llm(FakeLlm::new(vec!["Done."]))
        .with_tts(tts.clone());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let spoken: Vec<_> = runner.run(audio_of(&["a", "b"])).speech().collect().await;

    let audio: Vec<u8> = spoken
        .into_iter()
        .map(|chunk| chunk.expect("chunk"))
        .flat_map(|chunk| chunk.data.to_vec())
        .collect();
    assert_eq!(String::from_utf8(audio).expect("utf-8"), "Done.");

    // This response is a single sentence that ends with the token stream, so
    // it cannot be known complete until the model finishes — hence synthesis
    // after LlmFinished. When a boundary arrives mid-stream, speech starts
    // earlier; see `speech_starts_before_the_model_finishes`.
    //
    // Two chunks in, so two AudioChunkReceived: the capture events describe the
    // stream chunk by chunk rather than summarizing it once at the end.
    let events = drain(&mut subscription).await;
    assert_eq!(
        names(&events),
        [
            "ConversationStarted",
            "TurnStarted",
            "AudioStarted",
            "AudioChunkReceived",
            "AudioChunkReceived",
            "AudioFinished",
            "SpeechPartial",
            "SpeechFinal",
            "LlmRequestStarted",
            "LlmToken",
            "LlmFinished",
            "TtsStarted",
            "UtteranceSegmentStarted",
            "AudioStreaming",
            "TtsFinished",
            "ConversationCompleted",
        ]
    );
}

#[tokio::test]
async fn speech_starts_before_the_model_finishes() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        // The first sentence completes while the model is still generating.
        .with_llm(FakeLlm::new(vec!["One. ", "Two."]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    let events = names(&drain(&mut subscription).await);
    let started = events.iter().position(|name| name == "TtsStarted").expect("TtsStarted");
    let finished = events.iter().position(|name| name == "LlmFinished").expect("LlmFinished");
    assert!(started < finished, "speech must begin before generation ends, got {events:?}");
}

#[tokio::test]
async fn synthesized_audio_is_converted_to_the_requested_rate() {
    // A voice trained at 22.05 kHz played at 16 kHz is not an error anyone
    // hears as an error: it plays 1.38x too slow and about four semitones low,
    // which sounds like the assistant slowed way down.
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hello")]))
        .with_llm(FakeLlm::new(vec!["ok"]))
        .with_tts(FakeTts::new().speaking_at(22_050));

    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");
    let conversation = runner.run(audio_of(&["a"]));
    let spoken: usize = conversation
        .speech()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|chunk| chunk.expect("chunk").data.len())
        .sum();

    // One second in at 22.05 kHz has to be one second out at 16 kHz.
    let frames = spoken / 2;
    assert!(
        (frames as i64 - 16_000).abs() < 320,
        "expected about 16000 frames at the requested rate, got {frames}"
    );
}

#[tokio::test]
async fn passes_the_transcript_to_the_model() {
    let llm = FakeLlm::new(vec!["ok"]);
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("what time is it")]))
        .with_llm(llm.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    let requests = llm.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "fake-llm");
    assert_eq!(requests[0].messages.last().expect("message").content, "what time is it");
}

#[tokio::test]
async fn requests_the_model_the_provider_definition_configures() {
    // A node names a provider *definition*, and the model to request is one of
    // that definition's fields. Falling back to the node's provider id instead
    // silently asks for a model nobody configured — and ids cannot even spell
    // an `ollama`-style tag, so `qwen3:8b` would go out as `qwen3`.
    let llm = FakeLlm::new(vec!["ok"]).serving(&["qwen3:8b"]);
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hello")]))
        .with_llm(llm.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    assert_eq!(llm.requests()[0].model, "qwen3:8b");
}

#[tokio::test]
async fn speaks_each_sentence_as_soon_as_it_is_complete() {
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        // Sentence boundaries arrive mid-token-stream, not just at the end.
        .with_llm(FakeLlm::new(vec!["One", ". ", "Two", "."]))
        .with_tts(tts.clone());

    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    // Waiting for the model to finish before speaking would collapse this to
    // one call with the whole response.
    assert_eq!(tts.spoken(), ["One.", "Two."]);
}

#[tokio::test]
async fn stage_failures_reach_the_bus_and_the_caller() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FailingStt)
        .with_llm(FakeLlm::new(vec!["unused"]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let spoken: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    assert!(spoken.iter().any(Result::is_err), "caller must see the failure");

    let events = drain(&mut subscription).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::StageFailed { node, .. } if node == "stt"
        )),
        "expected a StageFailed naming the node: {:?}",
        names(&events)
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::ConversationCancelled { reason: CancelReason::Error }
        )),
        "expected the turn to be cancelled: {:?}",
        names(&events)
    );
}

/// The reason a turn's cancellation was published with, if it was cancelled.
fn cancelled_with(events: &[Event]) -> Option<CancelReason> {
    events.iter().find_map(|event| match event {
        Event::ConversationCancelled { reason } => Some(*reason),
        _ => None,
    })
}

#[tokio::test]
async fn an_explicit_stop_cancels_the_turn_as_user_requested() {
    // An operator has to be able to tell someone who interrupted from someone
    // whose connection died, so these must not share a reason.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let tts = HangingTts::new();
    let speaking = tts.speaking();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("tell me a long story")]))
        .with_llm(FakeLlm::new(vec!["Once upon a time. "]))
        .with_tts(tts);

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let conversation = runner.run(audio_of(&["a"]));

    // Interrupt a turn that is genuinely talking, not one that already ended.
    tokio::time::timeout(Duration::from_secs(5), speaking.notified())
        .await
        .expect("the turn starts speaking");
    conversation.stop.request();

    let events = drain(&mut subscription).await;
    assert_eq!(
        cancelled_with(&events),
        Some(CancelReason::UserRequested),
        "a stop must be reported as asked for: {:?}",
        names(&events)
    );
}

#[tokio::test]
async fn a_stop_asked_for_before_the_turn_speaks_still_ends_it() {
    // Nothing synchronizes a client's stop with the turn's progress, so one
    // that arrives during recognition must not be dropped on the floor.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(HangingTts::new());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let conversation = runner.run(audio_of(&["a"]));
    conversation.stop.request();

    let events = drain(&mut subscription).await;
    assert_eq!(
        cancelled_with(&events),
        Some(CancelReason::UserRequested),
        "{:?}",
        names(&events)
    );
}

#[tokio::test]
async fn a_stopped_turn_stops_producing_audio() {
    // Stopping has to feel like stopping: the audio stream must end rather than
    // the turn quietly continuing to synthesize.
    let bus = EventBus::default();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["One. ", "Two. ", "Three."]))
        .with_tts(SlowTts);

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let mut conversation = runner.run(audio_of(&["a"]));

    // Interrupt after the reply has started, so a truncated stream means the
    // stop cut it short rather than the turn never having begun.
    let first = tokio::time::timeout(Duration::from_secs(5), conversation.output.next())
        .await
        .expect("audio starts");
    assert!(first.is_some(), "expected the turn to speak before being stopped");
    conversation.stop.request();

    let rest = tokio::time::timeout(Duration::from_secs(5), conversation.speech().count())
        .await
        .expect("the audio stream ends");
    // `One. Two. Three.` is 14 spoken bytes, one chunk each.
    assert!(rest < 13, "expected a truncated reply, got {} more chunk(s)", rest);
}

#[tokio::test]
async fn a_listener_that_leaves_is_cancelled_as_disconnected() {
    // Dropping the audio is how a client vanishes. It says nothing about
    // whether anyone meant to interrupt, so it must not claim barge-in.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["One. ", "Two. ", "Three. ", "Four."]))
        .with_tts(SlowTts);

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let mut conversation = runner.run(audio_of(&["a"]));

    // Leave mid-reply. Leaving before it starts would end the turn through the
    // same path, but this is the case an operator actually sees.
    let _ = tokio::time::timeout(Duration::from_secs(5), conversation.output.next())
        .await
        .expect("audio starts");
    drop(conversation);

    let events = drain(&mut subscription).await;
    assert_eq!(
        cancelled_with(&events),
        Some(CancelReason::Disconnected),
        "{:?}",
        names(&events)
    );
}

#[tokio::test]
async fn nothing_the_runtime_does_reports_barge_in() {
    // The reason is reserved for voice detected over the assistant, which
    // nothing implements. Emitting it for anything else is what let a panel
    // counting interruptions quietly count dropped connections instead.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["One. ", "Two."]))
        .with_tts(SlowTts);
    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");

    // Every way a turn can end: a stop, a listener leaving, a failure, and a
    // reply nobody interrupts.
    let stopped = runner.run(audio_of(&["a"]));
    stopped.stop.request();
    let mut events = drain(&mut subscription).await;

    let dropped = runner.run(audio_of(&["a"]));
    drop(dropped.speech());
    events.extend(drain(&mut subscription).await);

    let completed = runner.run(audio_of(&["a"]));
    let _: Vec<_> = completed.speech().collect().await;
    events.extend(drain(&mut subscription).await);

    let failing = Providers::new()
        .with_stt(FailingStt)
        .with_llm(FakeLlm::new(vec!["unused"]))
        .with_tts(FakeTts::new());
    let failing_bus = EventBus::default();
    let mut failures = failing_bus.subscribe();
    let failing_runner =
        Runner::prepare(&linear_graph(), &failing, failing_bus).expect("graph is executable");
    let _: Vec<_> = failing_runner.run(audio_of(&["a"])).speech().collect().await;
    events.extend(drain(&mut failures).await);

    assert!(
        !events.iter().any(|event| matches!(
            event,
            Event::ConversationCancelled { reason: CancelReason::BargeIn }
        )),
        "nothing may report barge-in: {:?}",
        names(&events)
    );
}

#[tokio::test]
async fn several_tools_bound_to_one_core_are_executable() {
    // Two tools on the model, which is the fan-out the runtime does support:
    // tools requested together run together. Nothing is wired, because there
    // is no order among them for an edge to state.
    let graph = voice_graph("test")
        .source("test")
        .stt("fake-stt")
        .core("fake-llm")
        .tool("search")
        .tool("clock")
        .tts("fake-tts")
        .build();

    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tool(fakes::FakeTool::new("search", serde_json::json!({})))
        .with_tool(fakes::FakeTool::new("clock", serde_json::json!({})))
        .with_tts(FakeTts::new());

    Runner::prepare(&graph, &providers, EventBus::default())
        .expect("tools bound to a core are executable");
}

#[tokio::test]
async fn rejects_stages_it_cannot_execute() {
    // The graph model is deliberately wider than this runtime. A stage it
    // cannot run is refused at prepare time rather than skipped, so nobody
    // discovers the omission by speaking to a pipeline that ignores it.
    let graph = PipelineGraph::new("identified")
        .with_node(Node::source("mic", "test", Modality::Audio))
        .with_node(Node::speaker_id("who", "builtin"))
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("mic", "who"))
        .with_edge(Edge::new("who", "stt"))
        .with_edge(Edge::new("stt", "core"))
        .with_edge(Edge::new("core", "tts"));

    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tts(FakeTts::new());

    let error = Runner::prepare(&graph, &providers, EventBus::default())
        .expect_err("speaker identification is not executable yet");
    assert!(matches!(error, Error::Config(message) if message.contains("speaker_id")));
}

#[tokio::test]
async fn a_definition_serving_several_models_uses_the_first() {
    // Replaces a test that refused any model not in the served list. That
    // check could only ever fire against the node's provider id, which was
    // never a model name to begin with — the refusal it produced was the bug,
    // not the guard. A graph has no way to pick among several models yet, so
    // the definition's first is the one an operator can predict.
    let llm = FakeLlm::new(vec!["ok"]).serving(&["qwen3:8b", "llama3.2:3b"]);
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hello")]))
        .with_llm(llm.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    assert_eq!(llm.requests()[0].model, "qwen3:8b");
}

#[tokio::test]
async fn rejects_an_audio_encoding_a_provider_cannot_handle() {
    let providers = Providers::new()
        .with_stt(
            FakeStt::new(vec![]).accepting_encodings(&[Encoding::PcmS16Le, Encoding::Flac]),
        )
        .with_llm(FakeLlm::new(vec![]))
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");

    let error = runner
        .with_format(AudioFormat { encoding: Encoding::Opus, ..AudioFormat::DEFAULT })
        .expect_err("the recognizer does not accept opus capture");
    assert!(matches!(
        error,
        Error::Config(message)
            if message.contains("stt")
                && message.contains("fake-stt")
                && message.contains("Opus")
    ));
}

#[tokio::test]
async fn reports_providers_the_registry_does_not_know() {
    let graph = linear_graph();
    let providers = Providers::new().with_llm(FakeLlm::new(vec![])).with_tts(FakeTts::new());

    let error = Runner::prepare(&graph, &providers, EventBus::default())
        .expect_err("no speech-to-text provider is registered");
    assert!(matches!(error, Error::UnknownProvider(name) if name == "fake-stt"));
}

#[tokio::test]
async fn a_runner_can_serve_more_than_one_turn() {
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(tts.clone());

    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    assert_eq!(tts.spoken(), ["Hello.", "Hello."]);
}

#[tokio::test]
async fn the_returned_conversation_id_is_the_one_events_carry() {
    // Without this a caller cannot tell which events on the bus are theirs.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let conversation = runner.run(audio_of(&["a"]));
    let id = conversation.id;
    let _: Vec<_> = conversation.speech().collect().await;

    let envelope = subscription.recv().await.expect("event");
    assert_eq!(envelope.conversation, Some(id));
}

#[tokio::test]
async fn each_turn_gets_its_own_conversation() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus.clone()).expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;
    let first = subscription.recv().await.expect("event").conversation;

    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;
    let mut second = None;
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_secs(5), subscription.recv()).await
    {
        if envelope.conversation != first {
            second = envelope.conversation;
            break;
        }
    }

    assert!(first.is_some(), "events must carry a conversation");
    assert!(second.is_some(), "a second turn must not reuse the first conversation");
}

#[tokio::test]
async fn every_event_in_a_turn_shares_one_trace() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    let mut traces = Vec::new();
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_secs(5), subscription.recv()).await
    {
        let terminal = envelope.event.is_terminal();
        traces.push(envelope.trace);
        if terminal {
            break;
        }
    }

    assert!(traces.len() > 1, "expected a full turn of events");
    assert!(traces.windows(2).all(|pair| pair[0] == pair[1]), "trace must not change");
}

#[tokio::test]
async fn every_event_of_a_turn_names_the_device_it_came_from() {
    // What makes `/v1/events?device=` able to select one satellite. Every
    // event, not just the first: a filter that dropped the tail would show a
    // conversation that starts and never ends.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());

    let device = conduit_core::id::DeviceId::new();
    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let _: Vec<_> = runner.run_for_device(device, audio_of(&["a"])).speech().collect().await;

    let mut seen = 0;
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_secs(5), subscription.recv()).await
    {
        assert_eq!(
            envelope.device,
            Some(device),
            "{:?} lost the device that caused it",
            envelope.event
        );
        seen += 1;
        if envelope.event.is_terminal() {
            break;
        }
    }
    assert!(seen > 1, "expected a whole turn's worth of events, saw {seen}");
}

#[tokio::test]
async fn a_turn_nobody_authenticated_carries_no_device() {
    // A device is only ever set from a token, so an unauthenticated turn must
    // leave it empty rather than inventing an identity.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    let envelope = subscription.recv().await.expect("event");
    assert_eq!(envelope.device, None);
}
