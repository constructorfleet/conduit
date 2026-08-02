//! Pipeline graph fixtures for tests.
//!
//! Most tests that need a graph are not testing the graph — they are testing a
//! deadline, a tool call, or an HTTP status, and the pipeline is scenery. Those
//! tests should name the shape they want and let the wiring follow, so that a
//! change to how nodes and edges are spelled touches this module instead of
//! every fixture in the workspace.
//!
//! Tests whose subject *is* the graph — validation, ordering, refusals — should
//! keep building nodes and edges explicitly. Fixtures hide exactly the detail
//! those tests exist to pin down.

use crate::graph::{
    ConfirmPolicy, Edge, Modality, Node, PipelineGraph, ReasoningCore, ToolBinding,
};

/// Builds the voice pipeline shape and wires it in the canonical order.
///
/// Every stage is optional, because a fixture for "a pipeline missing its
/// recognizer" is as legitimate as a complete one, and a builder that refused
/// to express it would send those tests back to building nodes by hand.
pub struct VoiceGraph {
    name: String,
    source: Option<String>,
    stt: Option<String>,
    core: Option<String>,
    tools: Vec<String>,
    tts: Option<String>,
    sink: Option<String>,
}

/// Starts a voice pipeline fixture named `name`.
///
/// Nothing is wired until [`VoiceGraph::build`], so stages may be named in any
/// order.
#[must_use]
pub fn voice_graph(name: impl Into<String>) -> VoiceGraph {
    VoiceGraph {
        name: name.into(),
        source: None,
        stt: None,
        core: None,
        tools: Vec::new(),
        tts: None,
        sink: None,
    }
}

impl VoiceGraph {
    /// Adds a `mic` source served by `provider`.
    #[must_use]
    pub fn source(mut self, provider: impl Into<String>) -> Self {
        self.source = Some(provider.into());
        self
    }

    /// Adds an `stt` stage served by `provider`.
    #[must_use]
    pub fn stt(mut self, provider: impl Into<String>) -> Self {
        self.stt = Some(provider.into());
        self
    }

    /// Adds a `core` stage reasoning with `provider`.
    #[must_use]
    pub fn core(mut self, provider: impl Into<String>) -> Self {
        self.core = Some(provider.into());
        self
    }

    /// Binds a tool to the core.
    ///
    /// Nothing is wired: the model decides at runtime whether to call a tool,
    /// so there is no edge that could say when it runs. A builder with tools
    /// and no core has nowhere to put them, and drops them.
    #[must_use]
    pub fn tool(mut self, provider: impl Into<String>) -> Self {
        self.tools.push(provider.into());
        self
    }

    /// Adds a `tts` stage served by `provider`.
    #[must_use]
    pub fn tts(mut self, provider: impl Into<String>) -> Self {
        self.tts = Some(provider.into());
        self
    }

    /// Adds a `sink` served by `provider`.
    #[must_use]
    pub fn sink(mut self, provider: impl Into<String>) -> Self {
        self.sink = Some(provider.into());
        self
    }

    /// Assembles the graph, wiring each named stage to the next.
    ///
    /// Absent stages are skipped rather than defaulted, so the edges describe
    /// the stages the caller actually asked for.
    #[must_use]
    pub fn build(self) -> PipelineGraph {
        let mut graph = PipelineGraph::new(self.name);
        let mut spine: Vec<&str> = Vec::new();

        // Stages are built through their own constructors rather than from a
        // node kind, because a typed node carries settings a fixture has no
        // opinion about and the constructors are where those defaults live.
        //
        // The endpoints are audio because this is a *voice* pipeline: the
        // builder's name is the declaration. A fixture for a text pipeline is
        // a different shape and will say so.
        let stages = [
            (
                "mic",
                self.source
                    .as_ref()
                    .map(|provider| Node::source("mic", provider, Modality::Audio)),
            ),
            ("stt", self.stt.as_ref().map(|provider| Node::stt("stt", provider))),
            ("core", self.core.as_ref().map(|provider| core_node(provider, &self.tools))),
            ("tts", self.tts.as_ref().map(|provider| Node::tts("tts", provider))),
            (
                "sink",
                self.sink
                    .as_ref()
                    .map(|provider| Node::sink("sink", provider, Modality::Audio)),
            ),
        ];

        for (id, stage) in stages {
            if let Some(stage) = stage {
                graph = graph.with_node(stage);
                spine.push(id);
            }
        }

        for pair in spine.windows(2) {
            graph = graph.with_edge(Edge::new(pair[0], pair[1]));
        }

        graph
    }
}

/// A `core` node reasoning with `provider` and offered `tools`.
fn core_node(provider: &str, tools: &[String]) -> Node {
    let mut core = ReasoningCore::new(provider);
    core.tools = tools
        .iter()
        .map(|provider| ToolBinding {
            provider: provider.clone(),
            confirm: ConfirmPolicy::default(),
        })
        .collect();
    Node::Core { id: "core".to_owned(), core }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(graph: &PipelineGraph) -> Vec<&str> {
        graph.nodes.iter().map(|node| node.id().as_str()).collect()
    }

    fn wiring(graph: &PipelineGraph) -> Vec<(&str, &str)> {
        graph.edges.iter().map(|e| (e.from.as_str(), e.to.as_str())).collect()
    }

    fn tool_providers(graph: &PipelineGraph) -> Vec<&str> {
        graph
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Core { core, .. } => Some(core),
                _ => None,
            })
            .flat_map(|core| core.tools.iter())
            .map(|binding| binding.provider.as_str())
            .collect()
    }

    #[test]
    fn a_named_stage_chain_wires_in_order() {
        let graph =
            voice_graph("linear").stt("fake-stt").core("fake-llm").tts("fake-tts").build();

        assert_eq!(ids(&graph), ["stt", "core", "tts"]);
        assert_eq!(wiring(&graph), [("stt", "core"), ("core", "tts")]);
        graph.validate().expect("a complete chain is a valid pipeline");
    }

    #[test]
    fn absent_stages_are_skipped_rather_than_defaulted() {
        // A fixture that names no recognizer must not acquire one, or a test
        // asserting the runtime rejects a pipeline with no `stt` would pass
        // for the wrong reason.
        let graph = voice_graph("no stt").core("fake-llm").tts("fake-tts").build();

        assert_eq!(ids(&graph), ["core", "tts"]);
        assert_eq!(wiring(&graph), [("core", "tts")]);
    }

    #[test]
    fn source_and_sink_extend_the_same_chain() {
        let graph = voice_graph("captured")
            .source("test")
            .stt("fake-stt")
            .core("fake-llm")
            .tts("fake-tts")
            .sink("test")
            .build();

        assert_eq!(ids(&graph), ["mic", "stt", "core", "tts", "sink"]);
        assert_eq!(
            wiring(&graph),
            [("mic", "stt"), ("stt", "core"), ("core", "tts"), ("tts", "sink")]
        );
        graph.validate().expect("valid");
    }

    #[test]
    fn tools_are_bound_to_the_core_rather_than_wired_as_stages() {
        // A tool is not a stage between the model and synthesis: the model
        // decides at runtime whether to call it, so a topological position for
        // it would state an ordering that does not exist.
        let graph = voice_graph("tools")
            .stt("fake-stt")
            .core("fake-llm")
            .tool("search")
            .tool("clock")
            .tts("fake-tts")
            .build();

        assert_eq!(ids(&graph), ["stt", "core", "tts"]);
        assert_eq!(wiring(&graph), [("stt", "core"), ("core", "tts")]);
        assert_eq!(tool_providers(&graph), ["search", "clock"]);
        graph.validate().expect("valid");
    }

    #[test]
    fn binding_a_tool_leaves_the_model_wired_straight_to_synthesis() {
        // The edge the old tool fan-out removed. A pipeline's reply reaches
        // synthesis whether or not the model reached for anything on the way.
        let graph = voice_graph("tools")
            .stt("fake-stt")
            .core("fake-llm")
            .tool("search")
            .tts("fake-tts")
            .build();

        assert!(wiring(&graph).contains(&("core", "tts")));
    }

    #[test]
    fn tools_need_no_synthesizer_to_hang_off() {
        let graph =
            voice_graph("headless").core("fake-llm").tool("search").sink("test").build();

        assert_eq!(wiring(&graph), [("core", "sink")]);
        assert_eq!(tool_providers(&graph), ["search"]);
    }

    #[test]
    fn stages_may_be_named_in_any_order() {
        let ordered = voice_graph("x").stt("fake-stt").core("fake-llm").tts("fake-tts").build();
        let shuffled =
            voice_graph("x").tts("fake-tts").core("fake-llm").stt("fake-stt").build();

        assert_eq!(ordered, shuffled);
    }
}
