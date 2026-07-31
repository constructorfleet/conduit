//! Behaviour of one conversation turn, driven through fake providers.
//!
//! The fakes record what they were asked to do, so these tests describe the
//! runtime's contract — what reaches each stage, what reaches the bus, and in
//! what order — without depending on any real model.

mod fakes;

use std::time::Duration;

use conduit_core::bus::{EventBus, Subscription};
use conduit_core::event::{CancelReason, Event};
use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_core::Error;
use conduit_provider::stt::Transcript;
use conduit_runtime::{Providers, Runner};
use fakes::{audio_of, FailingStt, FakeLlm, FakeStt, FakeTts, HangingTts, SlowTts};
use futures_util::StreamExt;

/// mic -> stt -> llm -> tts, the shape the runtime can execute today.
fn linear_graph() -> PipelineGraph {
    PipelineGraph::new("test")
        .with_node(Node::new("mic", NodeKind::Source, "test"))
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(
            Node::new("llm", NodeKind::Llm, "fake-llm")
                .with_config(serde_json::json!({ "model": "fake-1" })),
        )
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("mic", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
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
    let spoken: Vec<_> = runner.run(audio_of(&["a", "b"])).audio.collect().await;

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
    let events = drain(&mut subscription).await;
    assert_eq!(
        names(&events),
        [
            "ConversationStarted",
            "SpeechPartial",
            "SpeechFinal",
            "LlmRequestStarted",
            "LlmToken",
            "LlmFinished",
            "TtsStarted",
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
    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;

    let events = names(&drain(&mut subscription).await);
    let started = events.iter().position(|name| name == "TtsStarted").expect("TtsStarted");
    let finished = events.iter().position(|name| name == "LlmFinished").expect("LlmFinished");
    assert!(started < finished, "speech must begin before generation ends, got {events:?}");
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
    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;

    let requests = llm.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "fake-1");
    assert_eq!(requests[0].messages.last().expect("message").content, "what time is it");
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
    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;

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
    let spoken: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;

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
    let first = tokio::time::timeout(Duration::from_secs(5), conversation.audio.next())
        .await
        .expect("audio starts");
    assert!(first.is_some(), "expected the turn to speak before being stopped");
    conversation.stop.request();

    let rest = tokio::time::timeout(Duration::from_secs(5), conversation.audio.count())
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
    let _ = tokio::time::timeout(Duration::from_secs(5), conversation.audio.next())
        .await
        .expect("audio starts");
    drop(conversation.audio);

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
    drop(dropped.audio);
    events.extend(drain(&mut subscription).await);

    let completed = runner.run(audio_of(&["a"]));
    let _: Vec<_> = completed.audio.collect().await;
    events.extend(drain(&mut subscription).await);

    let failing = Providers::new()
        .with_stt(FailingStt)
        .with_llm(FakeLlm::new(vec!["unused"]))
        .with_tts(FakeTts::new());
    let failing_bus = EventBus::default();
    let mut failures = failing_bus.subscribe();
    let failing_runner =
        Runner::prepare(&linear_graph(), &failing, failing_bus).expect("graph is executable");
    let _: Vec<_> = failing_runner.run(audio_of(&["a"])).audio.collect().await;
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
async fn tool_fan_out_is_executable() {
    // Two tools hanging off the model, which is the fan-out the runtime does
    // support: tools requested together run together. Routing them through a
    // `router` node is what it does not — see `tests/plan.rs`.
    let graph = linear_graph()
        .with_node(Node::new("search", NodeKind::Tool, "search"))
        .with_node(Node::new("clock", NodeKind::Tool, "clock"))
        .with_edge(Edge::new("llm", "search"))
        .with_edge(Edge::new("llm", "clock"))
        .with_edge(Edge::new("search", "tts"))
        .with_edge(Edge::new("clock", "tts"));

    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tool(fakes::FakeTool::new("search", serde_json::json!({})))
        .with_tool(fakes::FakeTool::new("clock", serde_json::json!({})))
        .with_tts(FakeTts::new());

    Runner::prepare(&graph, &providers, EventBus::default())
        .expect("tool fan-out is executable");
}

#[tokio::test]
async fn rejects_stages_it_cannot_execute() {
    let graph = linear_graph()
        .with_node(Node::new("memory", NodeKind::Memory, "builtin"))
        .with_edge(Edge::new("llm", "memory"))
        .with_edge(Edge::new("memory", "tts"));

    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tts(FakeTts::new());

    let error = Runner::prepare(&graph, &providers, EventBus::default())
        .expect_err("memory is not executable yet");
    assert!(matches!(error, Error::Config(message) if message.contains("memory")));
}

#[tokio::test]
async fn requires_a_model_on_the_language_model_node() {
    let graph = PipelineGraph::new("test")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(Node::new("llm", NodeKind::Llm, "fake-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tts(FakeTts::new());

    let error = Runner::prepare(&graph, &providers, EventBus::default())
        .expect_err("a model must be configured");
    assert!(matches!(error, Error::Config(message) if message.contains("model")));
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

    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;
    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;

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
    let _: Vec<_> = conversation.audio.collect().await;

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
    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;
    let first = subscription.recv().await.expect("event").conversation;

    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;
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
    let _: Vec<_> = runner.run(audio_of(&["a"])).audio.collect().await;

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
