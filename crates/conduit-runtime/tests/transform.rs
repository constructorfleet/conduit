//! What a transform node changes about a turn.
//!
//! The question every test here asks is which rendering a rewrite reached. A
//! transform is a node rather than a setting precisely so that a pipeline can
//! speak one thing and write another, and that only means something if the
//! runtime honours the edges.

mod fakes;

use std::time::Duration;

use conduit_core::bus::{EventBus, Subscription};
use conduit_core::event::Event;
use conduit_core::graph::{Edge, Modality, Node, PipelineGraph};
use conduit_runtime::{Providers, Reply, Runner};
use fakes::{FakeLlm, FakeTransform, FakeTts};
use futures_util::StreamExt;

/// Reads events until the turn ends.
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

/// chat -> core -> clean -> tts, with a text sink when `transcript` is set.
fn graph_with_transform(transcript: bool) -> PipelineGraph {
    let graph = PipelineGraph::new("cleaned")
        .with_node(Node::source("chat", "test", Modality::Text))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::transform("clean", "fake-transform"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("chat", "core"))
        .with_edge(Edge::new("core", "clean"))
        .with_edge(Edge::new("clean", "tts"));

    if transcript {
        graph
            .with_node(Node::sink("transcript", "test", Modality::Text))
            .with_edge(Edge::new("core", "transcript"))
    } else {
        graph
    }
}

/// Everything a turn wrote down, in order.
fn written(replies: Vec<conduit_core::Result<Reply>>) -> Vec<String> {
    replies
        .into_iter()
        .filter_map(|reply| match reply.expect("reply") {
            Reply::Text(text) => Some(text),
            Reply::Speech(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn a_transform_rewrites_what_reaches_the_synthesizer() {
    let synthesizer = FakeTts::new();
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["All done 🎉. "]))
        .with_transform(FakeTransform::new("fake-transform").removing("🎉"))
        .with_tts(synthesizer.clone());

    let runner = Runner::prepare(&graph_with_transform(false), &providers, EventBus::default())
        .expect("graph is executable");
    let _: Vec<_> = runner.run_text("did it work").speech().collect().await;

    assert_eq!(synthesizer.spoken(), ["All done ."]);
}

#[tokio::test]
async fn the_transcript_keeps_what_the_model_wrote_when_only_speech_is_transformed() {
    // The reason a transform is a node: markdown belongs in a transcript and
    // not in a voice, and the edges are where that is said.
    let cleaner = FakeTransform::new("fake-transform").replacing_with("spoken");
    let synthesizer = FakeTts::new();
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["**written**. "]))
        .with_transform(cleaner)
        .with_tts(synthesizer.clone());

    let runner = Runner::prepare(&graph_with_transform(true), &providers, EventBus::default())
        .expect("graph is executable");
    let replies: Vec<_> = runner.run_text("say something").output.collect().await;

    assert_eq!(written(replies), ["**written**."]);
    assert_eq!(synthesizer.spoken(), ["spoken"]);
}

#[tokio::test]
async fn a_transform_feeding_a_text_sink_rewrites_what_is_written() {
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["Secret 1234. "]))
        .with_transform(FakeTransform::new("fake-transform").removing("1234"));
    let graph = PipelineGraph::new("redacted")
        .with_node(Node::source("chat", "test", Modality::Text))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::transform("redact", "fake-transform"))
        .with_node(Node::sink("out", "test", Modality::Text))
        .with_edge(Edge::new("chat", "core"))
        .with_edge(Edge::new("core", "redact"))
        .with_edge(Edge::new("redact", "out"));

    let runner =
        Runner::prepare(&graph, &providers, EventBus::default()).expect("graph is executable");
    let replies: Vec<_> = runner.run_text("what is it").output.collect().await;

    assert_eq!(written(replies), ["Secret ."]);
}

#[tokio::test]
async fn chained_transforms_run_in_the_order_the_edges_wire_them() {
    let first = FakeTransform::new("first").removing("one");
    let second = FakeTransform::new("second").removing("two");
    let synthesizer = FakeTts::new();
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["one two three. "]))
        .with_transform(first.clone())
        .with_transform(second.clone())
        .with_tts(synthesizer.clone());
    let graph = PipelineGraph::new("chained")
        .with_node(Node::source("chat", "test", Modality::Text))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::transform("a", "first"))
        .with_node(Node::transform("b", "second"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("chat", "core"))
        .with_edge(Edge::new("core", "a"))
        .with_edge(Edge::new("a", "b"))
        .with_edge(Edge::new("b", "tts"));

    let runner =
        Runner::prepare(&graph, &providers, EventBus::default()).expect("graph is executable");
    let _: Vec<_> = runner.run_text("count").speech().collect().await;

    assert_eq!(first.seen(), ["one two three."]);
    assert_eq!(second.seen(), ["two three."], "the second sees the first's output");
    assert_eq!(synthesizer.spoken(), ["three."]);
}

#[tokio::test]
async fn a_segment_left_with_nothing_to_say_is_not_synthesized() {
    // A sentence that was only an emoji has no spoken form. Asking a
    // synthesizer for the audio of nothing is a request no provider owes an
    // answer to, so the segment is dropped and the next one is spoken.
    let synthesizer = FakeTts::new();
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["🎉. ", "It worked. "]))
        .with_transform(FakeTransform::new("fake-transform").removing("🎉."))
        .with_tts(synthesizer.clone());

    let runner = Runner::prepare(&graph_with_transform(false), &providers, EventBus::default())
        .expect("graph is executable");
    let _: Vec<_> = runner.run_text("well").speech().collect().await;

    assert_eq!(synthesizer.spoken(), ["It worked."]);
}

#[tokio::test]
async fn the_segment_event_reports_the_words_that_were_spoken() {
    // A reconstruction reads these events. Reporting what the model wrote,
    // when something else was said aloud, would describe a turn that did not
    // happen.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["**bold**. "]))
        .with_transform(FakeTransform::new("fake-transform").removing("**"))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&graph_with_transform(false), &providers, bus).expect("executable");
    let _: Vec<_> = runner.run_text("emphasise").speech().collect().await;

    let events = drain(&mut subscription).await;
    let Some(Event::UtteranceSegmentStarted { text, .. }) =
        events.iter().find(|event| matches!(event, Event::UtteranceSegmentStarted { .. }))
    else {
        panic!("a spoken segment is announced");
    };
    assert_eq!(text, "bold.");
}

#[tokio::test]
async fn a_failing_transform_stops_the_turn_and_names_its_node() {
    // Passing the segment through would deliver exactly what the transform was
    // put in the graph to prevent, so the turn ends instead.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let synthesizer = FakeTts::new();
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["Anything. "]))
        .with_transform(FakeTransform::new("fake-transform").failing())
        .with_tts(synthesizer.clone());

    let runner =
        Runner::prepare(&graph_with_transform(false), &providers, bus).expect("executable");
    let replies: Vec<_> = runner.run_text("go").output.collect().await;

    assert!(
        replies.iter().any(std::result::Result::is_err),
        "the caller is told the turn failed"
    );
    assert!(synthesizer.spoken().is_empty(), "nothing untransformed is spoken");

    let events = drain(&mut subscription).await;
    let Some(Event::StageFailed { node, .. }) =
        events.iter().find(|event| matches!(event, Event::StageFailed { .. }))
    else {
        panic!("the failing stage is reported");
    };
    assert_eq!(node, "clean", "the node an operator can find in their graph");
}

#[tokio::test]
async fn a_transform_naming_an_unregistered_provider_is_refused_at_prepare() {
    let providers =
        Providers::new().with_llm(FakeLlm::new(vec!["hi"])).with_tts(FakeTts::new());

    let error = Runner::prepare(&graph_with_transform(false), &providers, EventBus::default())
        .expect_err("an unregistered transform cannot run");

    assert!(error.to_string().contains("fake-transform"), "{error}");
}

#[tokio::test]
async fn a_transform_that_renders_nothing_is_refused_at_prepare() {
    // One edge is the difference between a rewrite that runs and one that
    // never will, which is not a difference to discover when the emoji come
    // out anyway.
    let providers = Providers::new()
        .with_llm(FakeLlm::new(vec!["hi"]))
        .with_transform(FakeTransform::new("fake-transform"))
        .with_tts(FakeTts::new());
    let dangling = PipelineGraph::new("dangling")
        .with_node(Node::source("chat", "test", Modality::Text))
        .with_node(Node::core("core", "fake-llm"))
        .with_node(Node::transform("clean", "fake-transform"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("chat", "core"))
        .with_edge(Edge::new("core", "clean"))
        .with_edge(Edge::new("core", "tts"));

    let error = Runner::prepare(&dangling, &providers, EventBus::default())
        .expect_err("a transform nothing renders through is a mistake");

    assert!(error.to_string().contains("clean"), "{error}");
}
