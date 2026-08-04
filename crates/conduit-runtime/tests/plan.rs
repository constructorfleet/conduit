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
use conduit_core::graph::{
    ConfirmPolicy, Edge, MemoryBinding, MemoryMode, Modality, ModelBinding, Node,
    PipelineGraph, ReasoningCore, ToolBinding,
};
use conduit_core::{Error, GraphError};
use conduit_runtime::plan::Plan;
use conduit_runtime::{Providers, Runner};
use fakes::{FakeLlm, FakeSpeaker, FakeStt, FakeTool, FakeTts, FakeWake};

/// A model node that names no model, so the provider chooses.
fn llm_node(id: &str, provider: &str) -> Node {
    Node::core(id, provider)
}

/// stt -> llm -> tts, correctly wired.
fn wired() -> PipelineGraph {
    PipelineGraph::new("wired")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

fn providers() -> Providers {
    Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tts(FakeTts::new())
}

/// The same, plus a detector and an identifier to resolve those stages against.
fn identifying_providers() -> Providers {
    providers()
        .with_wake(FakeWake::accepting_after(1))
        .with_speaker(FakeSpeaker::knowing(conduit_core::id::SpeakerId::new()))
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

/// A pipeline fed by typed words rather than by a microphone.
fn typed_input() -> PipelineGraph {
    PipelineGraph::new("typed")
        .with_node(Node::source("chat", "websocket", Modality::Text))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("chat", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

#[test]
fn a_pipeline_fed_by_text_resolves_without_a_recognizer() {
    // Nothing is listening, so requiring a recognizer would mean configuring
    // one that never runs before a pipeline could be saved at all.
    let providers = Providers::new().with_llm(FakeLlm::new(vec![])).with_tts(FakeTts::new());

    Runner::prepare(&typed_input(), &providers, EventBus::default())
        .expect("a text pipeline needs no recognizer");
}

#[test]
fn a_pipeline_fed_by_audio_still_requires_a_recognizer() {
    // The absence of a recognizer means text input, so a graph that captures
    // audio and never transcribes it must not quietly become a text pipeline.
    let deaf = PipelineGraph::new("deaf")
        .with_node(Node::source("mic", "websocket", Modality::Audio))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("mic", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let error = refusal(&deaf, &providers());
    assert!(
        matches!(error, Error::InvalidGraph(GraphError::ModalityMismatch { .. })),
        "audio reaching a model is a wiring error, not a missing stage: {error}"
    );
}

#[test]
fn a_pipeline_wired_backwards_is_refused_structurally() {
    // The defect this test exists for: `tts -> llm -> stt` used to resolve
    // identically to a correct graph, because resolution read only node kinds.
    // The refusal has since moved down into the graph, where it belongs — the
    // wiring is wrong on its own terms, not merely wrong for this runtime.
    let backwards = PipelineGraph::new("backwards")
        .with_node(Node::tts("tts", "fake-tts"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::stt("stt", "fake-stt"))
        .with_edge(Edge::new("tts", "llm"))
        .with_edge(Edge::new("llm", "stt"));

    let error = refusal(&backwards, &providers());
    let Error::InvalidGraph(GraphError::ModalityMismatch { from, to, .. }) = &error else {
        panic!("a backwards pipeline is an invalid graph, not a runtime limitation: {error}");
    };
    assert_eq!((from.as_str(), to.as_str()), ("tts", "llm"));
}

#[test]
fn a_graph_with_no_edges_is_refused_by_validation() {
    let unwired = PipelineGraph::new("unwired")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"));

    let error = refusal(&unwired, &providers());
    assert!(
        matches!(error, Error::InvalidGraph(GraphError::Disconnected(_))),
        "an unwired graph is not a pipeline: {error}"
    );
}

/// Recognition feeding the model and synthesis in parallel.
///
/// Every edge here is modality-compatible — synthesis renders written words as
/// readily as an utterance — so the graph itself is sound. What is wrong is
/// that the model's answer reaches nothing, while this runtime would speak it
/// anyway. Modality typing is a property of each edge; this is a property of a
/// path, so no per-edge rule can see it.
fn branches_past_the_model() -> PipelineGraph {
    PipelineGraph::new("sideways")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("stt", "tts"))
}

#[test]
fn synthesis_that_does_not_follow_the_model_is_refused() {
    // Both stages exist and both are wired to something, so only the *order*
    // is wrong. That used to be the runtime's complaint; it is the graph's
    // now, because a graph with one core can state the rule itself.
    let error = refusal(&branches_past_the_model(), &providers());
    let Error::InvalidGraph(GraphError::SinkMissesCore(node)) = &error else {
        panic!("a branch past the model is an invalid graph: {error}");
    };
    assert_eq!(node, "tts");
}

#[test]
fn every_edge_can_be_modality_compatible_while_the_path_is_wrong() {
    // Why core reachability had to exist: each edge here carries something
    // its far end reads, so no per-edge rule sees the defect. Without the
    // path rule this shape ran, and the person speaking heard their own words
    // read back, synthesized from the transcript, with the model's answer
    // discarded in silence.
    let graph = branches_past_the_model();
    assert!(
        graph.edges.iter().all(|edge| {
            let (Some(from), Some(to)) = (graph.node(&edge.from), graph.node(&edge.to)) else {
                return true;
            };
            from.output_modality()
                .is_none_or(|produced| to.accepted_modalities().contains(&produced))
        }),
        "every edge is compatible on its own terms"
    );
    assert!(graph.validate().is_err(), "and the graph is still wrong");
}

#[test]
fn a_stage_between_two_others_does_not_break_the_wiring() {
    // The check is reachability, not a direct edge, so a graph may grow a node
    // between two stages without being rewired. Only a kind that is not a
    // modality transform can sit between the model and synthesis: anything
    // that consumed the utterance would have to say what it produced.
    let through_a_tool = PipelineGraph::new("through")
        .with_node(Node::source("mic", "test", Modality::Audio))
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::speaker_id("who", "fake-speaker"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_node(Node::sink("speaker", "test", Modality::Audio))
        .with_edge(Edge::new("mic", "who"))
        .with_edge(Edge::new("who", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
        .with_edge(Edge::new("tts", "speaker"));

    Runner::prepare(&through_a_tool, &identifying_providers(), EventBus::default())
        .expect("a stage between two others is still one pipeline");
}

#[test]
fn a_stage_naming_a_detector_nobody_configured_is_refused() {
    // Refused at prepare time rather than at the first word: a pipeline that
    // names a detector the deployment does not have would otherwise look
    // healthy right up until nothing ever woke it.
    let graph = PipelineGraph::new("deaf")
        .with_node(Node::source("mic", "test", Modality::Audio))
        .with_node(Node::wake_word("wake", "openwakeword"))
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("mic", "wake"))
        .with_edge(Edge::new("wake", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let error = refusal(&graph, &providers());
    assert!(
        matches!(&error, Error::UnknownProvider(name) if name == "openwakeword"),
        "got {error}"
    );
}

#[test]
fn a_pipeline_can_wake_and_identify_in_the_same_graph() {
    let graph = PipelineGraph::new("attentive")
        .with_node(Node::source("mic", "test", Modality::Audio))
        .with_node(Node::wake_word("wake", "fake-wake"))
        .with_node(Node::speaker_id("who", "fake-speaker"))
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("mic", "wake"))
        .with_edge(Edge::new("wake", "who"))
        .with_edge(Edge::new("who", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let plan = Plan::resolve(&graph, &identifying_providers()).expect("resolves");
    let wake = plan.wake.expect("the wake stage resolved");
    assert_eq!(wake.node, "wake");
    assert_eq!(
        wake.phrases.iter().map(|phrase| phrase.phrase.as_str()).collect::<Vec<_>>(),
        ["hey jarvis"],
        "the phrases come from the detector's own definition"
    );
    assert_eq!(plan.speaker.expect("the identification stage resolved").node, "who");
}

#[test]
fn two_wake_stages_are_refused() {
    // One microphone, one activation. Two detectors scoring the same audio
    // would each open the gate, and which one woke the house would be a race.
    let graph = PipelineGraph::new("twice")
        .with_node(Node::source("mic", "test", Modality::Audio))
        .with_node(Node::wake_word("first", "fake-wake"))
        .with_node(Node::wake_word("second", "fake-wake"))
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("mic", "first"))
        .with_edge(Edge::new("first", "second"))
        .with_edge(Edge::new("second", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let message = config_message(&refusal(&graph, &identifying_providers())).to_owned();
    assert!(message.contains("wake_word"), "{message}");
}

#[test]
fn a_tool_bound_to_the_core_resolves() {
    // A tool is configuration on the core rather than a stage, so it needs no
    // edge and cannot be wired anywhere it would not be reached.
    let mut core = ReasoningCore::new("fake-llm");
    core.tools
        .push(ToolBinding { provider: "search".to_owned(), confirm: ConfirmPolicy::Never });
    let graph = PipelineGraph::new("bound")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "llm".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let providers = providers().with_tool(FakeTool::new("search", serde_json::json!({})));
    Runner::prepare(&graph, &providers, EventBus::default()).expect("executable");
}

#[test]
fn binding_an_mcp_server_offers_every_tool_it_registered() {
    // An MCP definition registers what its server advertises as
    // `<definition id>.<tool name>`, and the console offers the definition
    // itself as a tool to add. Naming it used to resolve to nothing, so a
    // pipeline built the obvious way failed with "no provider registered as
    // `weather`" — and the way around it was to list every tool by hand and
    // come back whenever the server grew one.
    let mut core = ReasoningCore::new("fake-llm");
    core.tools
        .push(ToolBinding { provider: "weather".to_owned(), confirm: ConfirmPolicy::Never });
    let graph = PipelineGraph::new("umbrella")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "llm".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let providers = providers()
        .with_tool(FakeTool::new("weather.forecast", serde_json::json!({})))
        .with_tool(FakeTool::new("weather.alerts", serde_json::json!({})))
        // A different server, which the binding must not sweep in.
        .with_tool(FakeTool::new("weatherhouse.lights", serde_json::json!({})));
    let plan = Plan::resolve(&graph, &providers).expect("executable");

    let mut offered: Vec<_> = plan.core.tool_specs().into_iter().map(|s| s.name).collect();
    offered.sort();
    assert_eq!(offered, vec!["weather.alerts", "weather.forecast"]);
}

#[test]
fn binding_a_name_that_is_neither_a_tool_nor_a_server_is_still_refused() {
    // Group expansion must not turn a typo into a pipeline that silently
    // offers the model nothing.
    let mut core = ReasoningCore::new("fake-llm");
    core.tools
        .push(ToolBinding { provider: "wether".to_owned(), confirm: ConfirmPolicy::Never });
    let graph = PipelineGraph::new("typo")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "llm".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let providers =
        providers().with_tool(FakeTool::new("weather.forecast", serde_json::json!({})));
    assert!(matches!(
        refusal(&graph, &providers),
        Error::UnknownProvider(name) if name == "wether"
    ));
}

#[test]
fn a_second_model_is_refused_by_validation_rather_than_by_this_runtime() {
    // This used to be a runtime limitation — "one `llm` per turn" — which said
    // the graph was fine and Conduit was not up to it. A pipeline that reasons
    // twice says nothing about which answer is the reply, so it is the graph
    // that is wrong, and the refusal belongs where every consumer sees it.
    let graph = PipelineGraph::new("two models")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("local", "fake-llm"))
        .with_node(llm_node("cloud", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "local"))
        .with_edge(Edge::new("stt", "cloud"))
        .with_edge(Edge::new("local", "tts"))
        .with_edge(Edge::new("cloud", "tts"));

    let error = refusal(&graph, &providers());
    let Error::InvalidGraph(GraphError::MultipleCores(nodes)) = &error else {
        panic!("two models is an invalid graph, not a runtime limitation: {error}");
    };
    assert_eq!(nodes, &["local".to_owned(), "cloud".to_owned()]);
}

/// stt -> core -> tts, with one tool bound to the core.
///
/// A core carrying every setting a pipeline can put on one: a named model, a
/// system prompt, a tool, and a round cap.
fn cored() -> PipelineGraph {
    let core = ReasoningCore {
        model: ModelBinding {
            provider: "fake-llm".to_owned(),
            model: Some("qwen3:8b".to_owned()),
            settings: serde_json::Map::new(),
        },
        system: Some("Be brief.".to_owned()),
        tools: vec![ToolBinding {
            provider: "search".to_owned(),
            confirm: ConfirmPolicy::Never,
        }],
        memory: Vec::new(),
        max_rounds: 2,
    };

    PipelineGraph::new("same pipeline")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "brain".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "brain"))
        .with_edge(Edge::new("brain", "tts"))
}

/// Providers for the shape above.
fn providers_with_a_tool() -> Providers {
    Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]).serving(&["qwen3:8b"]))
        .with_tool(FakeTool::new("search", serde_json::json!({})))
        .with_tts(FakeTts::new())
}

#[test]
fn a_cores_settings_all_reach_the_plan() {
    // Everything the runtime reads at turn time comes off the core plan, so a
    // setting that does not arrive here is a setting the operator wrote and
    // the pipeline ignored.
    let plan = Plan::resolve(&cored(), &providers_with_a_tool()).expect("a core is executable");

    assert_eq!(plan.core.node, "brain");
    assert_eq!(plan.core.model, "qwen3:8b");
    assert_eq!(plan.core.system.as_deref(), Some("Be brief."));
    assert_eq!(plan.core.max_rounds, 2);
    assert_eq!(plan.core.tool_specs().len(), 1, "the tool is offered to the model");
}

#[test]
fn a_core_names_the_model_it_asks_for_rather_than_taking_the_providers_first() {
    // The rule per-node configuration exists for, restated on the core: a
    // binding that names a model the provider does not serve is refused here
    // rather than at the first token.
    let core = ReasoningCore {
        model: ModelBinding {
            provider: "fake-llm".to_owned(),
            model: Some("llama3.2:3b".to_owned()),
            settings: serde_json::Map::new(),
        },
        ..ReasoningCore::new("fake-llm")
    };
    let graph = PipelineGraph::new("wrong model")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "brain".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "brain"))
        .with_edge(Edge::new("brain", "tts"));

    let message = config_message(&refusal(&graph, &providers_with_a_tool())).to_owned();
    assert!(message.contains("llama3.2:3b"), "the model asked for: {message}");
    assert!(message.contains("qwen3:8b"), "and what the provider serves: {message}");
}

#[test]
fn a_memory_binding_resolves_against_the_registered_store() {
    // This used to refuse: accepting a binding and not running it turns
    // "remember what I told you" into "answer as though nothing was said".
    // The runtime runs them now, so the binding resolves.
    let core = ReasoningCore {
        memory: vec![MemoryBinding {
            provider: "fake-memory".to_owned(),
            mode: MemoryMode::ReadWrite,
            scope: None,
            limit: 5,
        }],
        ..ReasoningCore::new("fake-llm")
    };
    let graph = PipelineGraph::new("remembering")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "brain".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "brain"))
        .with_edge(Edge::new("brain", "tts"));

    let providers = providers().with_memory(fakes::FakeMemory::new());
    Runner::prepare(&graph, &providers, EventBus::default())
        .expect("a bound memory store is executable");
}

#[test]
fn a_memory_binding_naming_no_registered_store_is_refused() {
    let core = ReasoningCore {
        memory: vec![MemoryBinding {
            provider: "missing".to_owned(),
            mode: MemoryMode::Read,
            scope: None,
            limit: 5,
        }],
        ..ReasoningCore::new("fake-llm")
    };
    let graph = PipelineGraph::new("remembering")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "brain".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "brain"))
        .with_edge(Edge::new("brain", "tts"));

    assert!(matches!(
        refusal(&graph, &providers()),
        Error::UnknownProvider(name) if name == "missing"
    ));
}

#[test]
fn a_tool_that_must_be_confirmed_resolves_and_is_asked_about_at_dispatch() {
    // `confirm: always` used to refuse the whole pipeline, because dispatching
    // a tool the operator asked to be consulted about is the one outcome the
    // setting was chosen to prevent. Now the question can be asked, so the
    // pipeline runs and the asking happens per call — see
    // `crates/conduit-runtime/tests/tools.rs`.
    let core = ReasoningCore {
        tools: vec![ToolBinding {
            provider: "search".to_owned(),
            confirm: ConfirmPolicy::Always,
        }],
        ..ReasoningCore::new("fake-llm")
    };
    let graph = PipelineGraph::new("gated")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::Core { id: "brain".to_owned(), core })
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "brain"))
        .with_edge(Edge::new("brain", "tts"));

    Runner::prepare(&graph, &providers_with_a_tool(), EventBus::default())
        .expect("a gated tool is executable now that it can be asked about");
}

#[test]
fn a_cores_bindings_do_not_have_to_be_downstream_of_anything() {
    // A binding is not a stage, so there is no edge for it to be reachable
    // over. The old shape needed `llm -> search`; the core needs nothing.
    let graph = cored();
    assert!(
        !graph.edges.iter().any(|edge| edge.to == "search"),
        "a bound tool is not wired into the transport pipeline"
    );
    Plan::resolve(&graph, &providers_with_a_tool()).expect("executable all the same");
}

#[test]
fn a_missing_stage_is_named() {
    let no_synthesis = PipelineGraph::new("mute")
        .with_node(Node::stt("stt", "fake-stt"))
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
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_edge(Edge::new("stt", "llm"));

    let message = config_message(&refusal(&no_synthesis, &providers())).to_owned();
    assert!(!message.contains("downstream"), "{message}");
}

/// A node's overrides, from the JSON an operator would have written.
fn settings(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().expect("an object").clone()
}

/// The same wired pipeline, with each stage overriding one setting.
fn overriding() -> PipelineGraph {
    let core = ReasoningCore {
        model: ModelBinding {
            provider: "fake-llm".to_owned(),
            model: None,
            settings: settings(serde_json::json!({ "style": "warm" })),
        },
        ..ReasoningCore::new("fake-llm")
    };

    PipelineGraph::new("overriding")
        .with_node(Node::Stt {
            id: "stt".to_owned(),
            provider: "fake-stt".to_owned(),
            settings: settings(serde_json::json!({ "style": "plain" })),
        })
        .with_node(Node::Core { id: "llm".to_owned(), core })
        .with_node(Node::Tts {
            id: "tts".to_owned(),
            provider: "fake-tts".to_owned(),
            voice: None,
            settings: settings(serde_json::json!({ "style": "warm" })),
        })
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

/// Providers that all declare the same one-setting schema.
fn providers_accepting_settings() -> Providers {
    Providers::new()
        .with_stt(FakeStt::new(vec![]).accepting_settings(fakes::style_schema()))
        .with_llm(FakeLlm::new(vec![]).accepting_settings(fakes::style_schema()))
        .with_tts(FakeTts::new().accepting_settings(fakes::style_schema()))
}

#[test]
fn a_nodes_overrides_reach_the_plan_for_every_stage_that_takes_them() {
    // Overriding is what lets two pipelines share one Configured Provider and
    // still ask it for different things, so an override that does not arrive
    // here is one the operator wrote and the pipeline ignored.
    let plan =
        Plan::resolve(&overriding(), &providers_accepting_settings()).expect("executable");

    assert_eq!(
        plan.stt.as_ref().expect("a recognizer").settings.string("style"),
        Some("plain")
    );
    assert_eq!(plan.core.settings.string("style"), Some("warm"));
    assert_eq!(
        plan.tts.as_ref().expect("a synthesizer").settings.string("style"),
        Some("warm")
    );
}

#[test]
fn a_node_that_overrides_nothing_leaves_the_configured_provider_in_charge() {
    // The reason resolution checks overrides rather than whole settings
    // objects: filling in the schema's declared defaults here would displace
    // whatever the operator configured on the Configured Provider itself.
    let plan = Plan::resolve(&wired(), &providers_accepting_settings()).expect("executable");

    assert!(plan.stt.as_ref().expect("a recognizer").settings.is_empty());
    assert!(plan.core.settings.is_empty());
    assert!(plan.tts.as_ref().expect("a synthesizer").settings.is_empty());
}

#[test]
fn an_override_the_provider_does_not_accept_is_refused_at_prepare_time() {
    // A setting the provider never declared used to travel to the backend and
    // be ignored there. Refusing it here names the node, which is where the
    // operator has to go to fix it.
    let graph = PipelineGraph::new("mistyped")
        .with_node(Node::Stt {
            id: "stt".to_owned(),
            provider: "fake-stt".to_owned(),
            settings: settings(serde_json::json!({ "styl": "plain" })),
        })
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let message = config_message(&refusal(&graph, &providers_accepting_settings())).to_owned();
    assert!(message.contains("styl"), "the setting that is wrong: {message}");
    assert!(message.contains("stt"), "and the node to fix it on: {message}");
}

#[test]
fn an_override_outside_a_declared_enum_is_refused() {
    let graph = PipelineGraph::new("out of range")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(llm_node("llm", "fake-llm"))
        .with_node(Node::Tts {
            id: "tts".to_owned(),
            provider: "fake-tts".to_owned(),
            voice: None,
            settings: settings(serde_json::json!({ "style": "shouty" })),
        })
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"));

    let message = config_message(&refusal(&graph, &providers_accepting_settings())).to_owned();
    assert!(message.contains("shouty"), "{message}");
}
