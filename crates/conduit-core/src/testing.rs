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

use crate::graph::{Edge, Node, PipelineGraph};

/// Builds the voice pipeline shape and wires it in the canonical order.
///
/// Every stage is optional, because a fixture for "a pipeline missing its
/// recognizer" is as legitimate as a complete one, and a builder that refused
/// to express it would send those tests back to building nodes by hand.
pub struct VoiceGraph {
    name: String,
    source: Option<String>,
    stt: Option<String>,
    llm: Option<String>,
    tools: Vec<(String, String)>,
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
        llm: None,
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

    /// Adds an `llm` stage served by `provider`.
    #[must_use]
    pub fn llm(mut self, provider: impl Into<String>) -> Self {
        self.llm = Some(provider.into());
        self
    }

    /// Adds a tool node hanging off the model.
    ///
    /// Tools fan out in parallel rather than chaining, because that is the
    /// topology the runtime executes: tools requested in one model round run
    /// together.
    #[must_use]
    pub fn tool(mut self, id: impl Into<String>, provider: impl Into<String>) -> Self {
        self.tools.push((id.into(), provider.into()));
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
        let stages = [
            ("mic", self.source.as_ref().map(|provider| Node::source("mic", provider))),
            ("stt", self.stt.as_ref().map(|provider| Node::stt("stt", provider))),
            ("llm", self.llm.as_ref().map(|provider| Node::llm("llm", provider))),
            ("tts", self.tts.as_ref().map(|provider| Node::tts("tts", provider))),
            ("sink", self.sink.as_ref().map(|provider| Node::sink("sink", provider))),
        ];

        for (id, stage) in stages {
            if let Some(stage) = stage {
                graph = graph.with_node(stage);
                spine.push(id);
            }
        }

        for (id, provider) in &self.tools {
            graph = graph.with_node(Node::tool(id.clone(), provider.clone()));
        }

        // The model's successor is where every tool branch rejoins. Finding it
        // by position keeps the fan-out correct for a fixture that has no
        // synthesizer, where tools rejoin at the sink instead.
        let after_llm = spine
            .iter()
            .position(|&id| id == "llm")
            .and_then(|llm| spine.get(llm + 1).copied());

        for pair in spine.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            let branches_through_tools =
                from == "llm" && !self.tools.is_empty() && Some(to) == after_llm;
            if !branches_through_tools {
                graph = graph.with_edge(Edge::new(from, to));
            }
        }

        for (id, _) in &self.tools {
            if self.llm.is_some() {
                graph = graph.with_edge(Edge::new("llm", id.clone()));
            }
            if let Some(rejoin) = after_llm {
                graph = graph.with_edge(Edge::new(id.clone(), rejoin));
            }
        }

        graph
    }
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

    #[test]
    fn a_named_stage_chain_wires_in_order() {
        let graph =
            voice_graph("linear").stt("fake-stt").llm("fake-llm").tts("fake-tts").build();

        assert_eq!(ids(&graph), ["stt", "llm", "tts"]);
        assert_eq!(wiring(&graph), [("stt", "llm"), ("llm", "tts")]);
        graph.validate().expect("a complete chain is a valid pipeline");
    }

    #[test]
    fn absent_stages_are_skipped_rather_than_defaulted() {
        // A fixture that names no recognizer must not acquire one, or a test
        // asserting the runtime rejects a pipeline with no `stt` would pass
        // for the wrong reason.
        let graph = voice_graph("no stt").llm("fake-llm").tts("fake-tts").build();

        assert_eq!(ids(&graph), ["llm", "tts"]);
        assert_eq!(wiring(&graph), [("llm", "tts")]);
    }

    #[test]
    fn source_and_sink_extend_the_same_chain() {
        let graph = voice_graph("captured")
            .source("test")
            .stt("fake-stt")
            .llm("fake-llm")
            .tts("fake-tts")
            .sink("test")
            .build();

        assert_eq!(ids(&graph), ["mic", "stt", "llm", "tts", "sink"]);
        assert_eq!(
            wiring(&graph),
            [("mic", "stt"), ("stt", "llm"), ("llm", "tts"), ("tts", "sink")]
        );
        graph.validate().expect("valid");
    }

    #[test]
    fn tools_fan_out_from_the_model_rather_than_chaining() {
        // Chaining them would say the second tool runs after the first, which
        // is not what the runtime does with one round's requests.
        let graph = voice_graph("tools")
            .stt("fake-stt")
            .llm("fake-llm")
            .tool("search", "search")
            .tool("clock", "clock")
            .tts("fake-tts")
            .build();

        assert_eq!(ids(&graph), ["stt", "llm", "tts", "search", "clock"]);
        assert_eq!(
            wiring(&graph),
            [
                ("stt", "llm"),
                ("llm", "search"),
                ("search", "tts"),
                ("llm", "clock"),
                ("clock", "tts"),
            ]
        );
        graph.validate().expect("valid");
    }

    #[test]
    fn a_tool_graph_does_not_also_wire_the_model_straight_to_synthesis() {
        // Leaving that edge in place would describe a pipeline that reaches
        // synthesis without the tools, which is a different topology.
        let graph = voice_graph("tools")
            .stt("fake-stt")
            .llm("fake-llm")
            .tool("search", "search")
            .tts("fake-tts")
            .build();

        assert!(!wiring(&graph).contains(&("llm", "tts")));
    }

    #[test]
    fn tools_rejoin_at_the_sink_when_there_is_no_synthesizer() {
        let graph = voice_graph("headless")
            .llm("fake-llm")
            .tool("search", "search")
            .sink("test")
            .build();

        assert_eq!(wiring(&graph), [("llm", "search"), ("search", "sink")]);
    }

    #[test]
    fn stages_may_be_named_in_any_order() {
        let ordered = voice_graph("x").stt("fake-stt").llm("fake-llm").tts("fake-tts").build();
        let shuffled = voice_graph("x").tts("fake-tts").llm("fake-llm").stt("fake-stt").build();

        assert_eq!(ordered, shuffled);
    }
}
