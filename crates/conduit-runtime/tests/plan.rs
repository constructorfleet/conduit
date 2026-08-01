//! What the runtime will and will not accept as a pipeline.
//!
//! Resolution is where a graph stops being a drawing and becomes the thing
//! that runs, so the interesting cases are the ones where the two could
//! disagree: a graph whose edges describe an order the runtime would not
//! follow, and a node the graph model can express but the runtime cannot
//! execute. Both must be refused at prepare time rather than discovered when
//! someone speaks.

mod fakes;

use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
use conduit_core::{Error, GraphError};
use conduit_runtime::{Providers, Runner};
use fakes::{FakeLlm, FakeStt, FakeTool, FakeTts};
use futures_util::StreamExt;

/// A model node with the configuration every pipeline needs.
fn llm_node(id: &str, provider: &str) -> Node {
    Node::new(id, NodeKind::Llm, provider).with_config(serde_json::json!({ "model": "fake-1" }))
}

/// stt -> llm -> tts, correctly wired.
fn wired() -> PipelineGraph {
    PipelineGraph::new("wired")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

fn providers() -> Providers {
    Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tts(FakeTts::new())
}

fn providers_without_tts() -> Providers {
    Providers::new().with_stt(FakeStt::new(vec![])).with_llm(FakeLlm::new(vec![]))
}

/// Resolves `graph` and returns the error, failing if it resolves.
fn refusal(graph: &PipelineGraph, providers: &Providers) -> Error {
    match Runner::prepare(graph, providers, EventBus::default()) {
        Ok(_) => panic!("this graph should not have resolved"),
        Err(error) => error,
    }
}

/// The `Error::Config` message, for asserting on what an operator reads.
fn config_message(error: &Error) -> &str {
    match error {
        Error::Config(message) => message,
        other => panic!("expected a configuration error, got {other}"),
    }
}

#[test]
fn a_correctly_wired_pipeline_resolves() {
    Runner::prepare(&wired(), &providers(), EventBus::default()).expect("executable");
}

#[test]
fn a_pipeline_wired_backwards_is_refused() {
    // The defect this test exists for: `tts -> llm -> stt` used to resolve
    // identically to a correct graph, because resolution read only node kinds.
    let backwards = PipelineGraph::new("backwards")
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_edge(Edge::new("tts", "llm"))
        .with_edge(Edge::new("llm", "stt"));

    let error = refusal(&backwards, &providers());
    let message = config_message(&error);
    assert!(message.contains("stt"), "the message must name the stages: {message}");
    assert!(message.contains("llm"), "{message}");
}

#[test]
fn a_graph_with_no_edges_is_refused_by_validation() {
    let unwired = PipelineGraph::new("unwired")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"));

    let error = refusal(&unwired, &providers());
    assert!(
        matches!(error, Error::InvalidGraph(GraphError::Disconnected(_))),
        "an unwired graph is not a pipeline: {error}"
    );
}

#[test]
fn synthesis_that_does_not_follow_the_model_is_refused() {
    // Both stages exist and both are wired to something, so only the *order*
    // is wrong — which is exactly the case node-kind matching could not see.
    let sideways = PipelineGraph::new("sideways")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("stt", "tts"));

    let message = config_message(&refusal(&sideways, &providers())).to_owned();
    assert!(message.contains("tts"), "{message}");
}

#[test]
fn a_stage_between_two_others_does_not_break_the_wiring() {
    // The check is reachability, not a direct edge, so a graph may grow a node
    // between two stages without being rewired.
    let through_a_source = PipelineGraph::new("through")
        .with_node(Node::new("mic", NodeKind::Source, "test"))
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::new("sink", NodeKind::Sink, "test"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("mic", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "sink"))
        .with_edge(Edge::new("sink", "tts"));

    Runner::prepare(&through_a_source, &providers(), EventBus::default())
        .expect("an intervening node is not a break in the chain");
}

#[test]
fn a_tool_the_model_does_not_reach_is_refused() {
    let dangling_tool = PipelineGraph::new("dangling tool")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::new("search", NodeKind::Tool, "search"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
        // The tool hangs off the recognizer, so the graph says the model never
        // reaches it — while the runtime would offer it to the model anyway.
        .with_edge(Edge::new("search", "stt"));

    let providers = providers().with_tool(FakeTool::new("search", serde_json::json!({})));
    let message = config_message(&refusal(&dangling_tool, &providers)).to_owned();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn a_tool_downstream_of_the_model_resolves() {
    let graph = wired()
        .with_node(Node::new("search", NodeKind::Tool, "search"))
        .with_edge(Edge::new("llm", "search"))
        .with_edge(Edge::new("search", "tts"));

    let providers = providers().with_tool(FakeTool::new("search", serde_json::json!({})));
    Runner::prepare(&graph, &providers, EventBus::default()).expect("executable");
}

#[test]
fn a_router_node_is_refused_rather_than_ignored() {
    // Accepting a router and then ignoring it turns "ask the cloud model the
    // hard questions" into "ask whichever model resolved", silently.
    let graph = wired()
        .with_node(Node::new("route", NodeKind::Router, "builtin"))
        .with_edge(Edge::new("llm", "route"))
        .with_edge(Edge::new("route", "tts"));

    let message = config_message(&refusal(&graph, &providers())).to_owned();
    assert!(message.contains("router"), "the message must name the kind: {message}");
    assert!(message.contains("route"), "and the node: {message}");
}

/// The shape `conduit-core`'s `router_fan_out_joins_before_the_sink` asserts
/// valid, resolved against this runtime.
///
/// The two layers disagree on purpose — the graph model is the wider of them —
/// and this test is where that disagreement is written down rather than
/// discovered.
#[test]
fn the_router_fan_out_a_valid_graph_describes_is_not_executable() {
    let graph = PipelineGraph::new("router")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(Node::new("router", NodeKind::Router, "builtin"))
        .with_node(llm_node("local", "fake-llm"))
        .with_node(llm_node("cloud", "other-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("stt", "router"))
        .with_edge(Edge::from_port("router", "local", "local"))
        .with_edge(Edge::from_port("router", "cloud", "cloud"))
        .with_edge(Edge::new("local", "tts"))
        .with_edge(Edge::new("cloud", "tts"));

    graph.validate().expect("a valid graph, as conduit-core's own test asserts");

    // The router is reached first, so that is what the operator is told about.
    let message = config_message(&refusal(&graph, &providers())).to_owned();
    assert!(message.contains("router"), "{message}");
}

#[test]
fn a_second_model_is_still_refused_as_a_duplicate() {
    let graph = PipelineGraph::new("two models")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("local", "fake-llm"))
        .with_node(llm_node("cloud", "fake-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "fake-tts"))
        .with_edge(Edge::new("stt", "local"))
        .with_edge(Edge::new("stt", "cloud"))
        .with_edge(Edge::new("local", "tts"))
        .with_edge(Edge::new("cloud", "tts"));

    let message = config_message(&refusal(&graph, &providers())).to_owned();
    assert!(message.contains("one per turn"), "{message}");
}

#[test]
fn a_missing_stage_is_named() {
    let no_synthesis = PipelineGraph::new("mute")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_edge(Edge::new("stt", "llm"));

    let message = config_message(&refusal(&no_synthesis, &providers())).to_owned();
    assert!(message.contains("tts"), "{message}");
}

#[test]
fn a_missing_stage_is_reported_before_a_wiring_complaint() {
    // "no `tts` node" is the actionable message; complaining that a node which
    // does not exist is not downstream of one that does would send its author
    // looking for the wrong problem.
    let no_synthesis = PipelineGraph::new("mute")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_edge(Edge::new("stt", "llm"));

    let message = config_message(&refusal(&no_synthesis, &providers())).to_owned();
    assert!(!message.contains("downstream"), "{message}");
}

#[test]
fn an_inline_wyoming_tts_provider_resolves_from_node_configuration() {
    let graph = PipelineGraph::new("inline wyoming")
        .with_node(Node::new("stt", NodeKind::Stt, "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::new("tts", NodeKind::Tts, "piper").with_config(serde_json::json!({
            "component": "wyoming.tts",
            "url": "tcp://127.0.0.1:10200",
            "voice": "en_US-ryan-high"
        })))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    Runner::prepare(&graph, &providers_without_tts(), EventBus::default())
        .expect("inline provider config is executable");
}

#[tokio::test]
async fn wyoming_tts_sends_voice_and_streams_audio_chunks() {
    use conduit_provider::tts::{SynthesisRequest, TextToSpeech};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read request");
        let request: serde_json::Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(request["type"], "synthesize");
        assert_eq!(request["data"]["text"], "hello");
        assert_eq!(request["data"]["voice"]["name"], "en_US-ryan-high");
        let mut socket = reader.into_inner();
        socket
            .write_all(
                br#"{"type":"audio-chunk","data":{"rate":16000,"width":2,"channels":1},"payload_length":4}
abcd"#,
            )
            .await
            .expect("audio chunk");
        socket.write_all(br#"{"type":"audio-stop"}"#).await.expect("audio stop");
        socket.write_all(b"\n").await.expect("newline");
    });
    let provider = conduit_runtime::wyoming::WyomingTts::from_inline(
        "piper",
        conduit_runtime::wyoming::InlineProviderConfig {
            component: Some("wyoming.tts".to_owned()),
            url: Some(format!("tcp://{address}")),
            voice: Some("en_US-ryan-high".to_owned()),
            ..Default::default()
        },
    )
    .expect("provider");

    let mut audio =
        provider.synthesize(SynthesisRequest::new("hello")).await.expect("synthesizes");
    let chunk = audio.next().await.expect("chunk").expect("ok");

    assert_eq!(chunk.data.as_ref(), b"abcd");
    assert_eq!(chunk.format.sample_rate, 16_000);
    server.await.expect("server finishes");
}
