//! The pipeline graph: a validated, serializable description of one voice
//! pipeline.
//!
//! A graph is data, not code. The web UI edits it, the API stores it, and the
//! runtime executes it — none of which requires recompiling Conduit or
//! knowing which providers a node refers to.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::error::GraphError;

/// A node's identifier within its graph. Unique per graph, chosen by the
/// author rather than generated, so graphs stay diffable.
pub type NodeId = String;

/// What a node does in the pipeline.
///
/// The kind determines the shape of a node's inputs and outputs; the
/// `provider` field on [`Node`] determines who implements it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NodeKind {
    /// Audio ingress from a device.
    Source,
    /// Wake word detection.
    WakeWord,
    /// Speech-to-text.
    Stt,
    /// Speaker identification.
    SpeakerId,
    /// Conditional fan-out to one of several downstream branches.
    Router,
    /// Language model inference.
    Llm,
    /// Tool execution.
    Tool,
    /// Memory read/write.
    Memory,
    /// Text-to-speech.
    Tts,
    /// Audio egress to a device.
    Sink,
}

/// One stage in a pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Author-chosen identifier, unique within the graph.
    pub id: NodeId,
    /// What this node does.
    pub kind: NodeKind,
    /// Registered provider name, e.g. `"whisper"` or `"piper"`.
    pub provider: String,
}

impl Node {
    /// Creates a graph node that selects a registered provider by id.
    #[must_use]
    pub fn new(id: impl Into<NodeId>, kind: NodeKind, provider: impl Into<String>) -> Self {
        Self { id: id.into(), kind, provider: provider.into() }
    }
}

/// A directed connection between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Id of the upstream node.
    pub from: NodeId,
    /// Id of the downstream node.
    pub to: NodeId,
    /// Named output port on `from`, used by multi-output nodes such as
    /// [`NodeKind::Router`]. `None` selects the node's default output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
}

impl Edge {
    /// Connects `from`'s default output to `to`.
    #[must_use]
    pub fn new(from: impl Into<NodeId>, to: impl Into<NodeId>) -> Self {
        Self { from: from.into(), to: to.into(), port: None }
    }

    /// Connects a named output port on `from` to `to`.
    #[must_use]
    pub fn from_port(
        from: impl Into<NodeId>,
        port: impl Into<String>,
        to: impl Into<NodeId>,
    ) -> Self {
        Self { from: from.into(), to: to.into(), port: Some(port.into()) }
    }
}

/// A complete pipeline definition.
///
/// Construct freely, then call [`PipelineGraph::validate`] before executing.
/// Deserialization does *not* validate — that keeps a malformed graph
/// loadable and therefore fixable in the editor.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PipelineGraph {
    /// Human-readable name shown in the UI.
    pub name: String,
    /// The stages in this pipeline, in declaration order.
    pub nodes: Vec<Node>,
    /// Connections between stages.
    pub edges: Vec<Edge>,
}

impl PipelineGraph {
    /// Creates an empty graph with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), nodes: Vec::new(), edges: Vec::new() }
    }

    /// Adds a node.
    #[must_use]
    pub fn with_node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self
    }

    /// Adds an edge.
    #[must_use]
    pub fn with_edge(mut self, edge: Edge) -> Self {
        self.edges.push(edge);
        self
    }

    /// Looks up a node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Checks that the graph is executable.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] if node ids collide, an edge dangles, the graph
    /// is cyclic, it lacks an entry or exit point, or it is not one connected
    /// pipeline.
    pub fn validate(&self) -> Result<(), GraphError> {
        self.topological_order().map(|_| ())
    }

    /// Whether `to` is downstream of `from`, following edges.
    ///
    /// This is what makes an edge mean something: a runtime that needs the
    /// recognizer to feed the model can ask whether it does, rather than
    /// assuming it because both nodes exist.
    ///
    /// A node does not reach itself unless a path leads back to it.
    #[must_use]
    pub fn reaches(&self, from: &str, to: &str) -> bool {
        let Ok(index) = self.index_nodes() else { return false };
        let (Some(&start), Some(&target)) = (index.get(from), index.get(to)) else {
            return false;
        };

        let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for edge in &self.edges {
            if let (Some(&a), Some(&b)) =
                (index.get(edge.from.as_str()), index.get(edge.to.as_str()))
            {
                outgoing[a].push(b);
            }
        }

        let mut seen = vec![false; self.nodes.len()];
        let mut queue: VecDeque<usize> = outgoing[start].iter().copied().collect();
        while let Some(current) = queue.pop_front() {
            if current == target {
                return true;
            }
            if std::mem::replace(&mut seen[current], true) {
                continue;
            }
            queue.extend(outgoing[current].iter().copied());
        }
        false
    }

    /// Returns the nodes in execution order.
    ///
    /// Ties are broken by declaration order, so the result is stable across
    /// runs and across serialization round-trips.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] under the same conditions as
    /// [`PipelineGraph::validate`].
    pub fn topological_order(&self) -> Result<Vec<&Node>, GraphError> {
        let index = self.index_nodes()?;

        let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        let mut indegree = vec![0_usize; self.nodes.len()];
        let mut has_outgoing = vec![false; self.nodes.len()];

        for edge in &self.edges {
            let from = *index
                .get(edge.from.as_str())
                .ok_or_else(|| GraphError::UnknownNode(edge.from.clone()))?;
            let to = *index
                .get(edge.to.as_str())
                .ok_or_else(|| GraphError::UnknownNode(edge.to.clone()))?;
            outgoing[from].push(to);
            incoming[to].push(from);
            has_outgoing[from] = true;
            indegree[to] += 1;
        }

        let sources: Vec<usize> = (0..self.nodes.len()).filter(|&i| indegree[i] == 0).collect();
        if sources.is_empty() {
            return Err(GraphError::NoSource);
        }
        if !has_outgoing.iter().any(|&out| !out) {
            return Err(GraphError::NoSink);
        }

        // A graph that is sound but not connected describes several pipelines,
        // or — when it has no edges at all — none. Every node then has
        // indegree 0 and no outgoing edge, so both checks above pass while
        // nothing says what feeds what.
        let islands = self.disconnected_nodes(&outgoing, &incoming);
        if !islands.is_empty() {
            return Err(GraphError::Disconnected(islands));
        }

        let mut queue: VecDeque<usize> = sources.into_iter().collect();
        let mut ordered = Vec::with_capacity(self.nodes.len());
        let mut placed = vec![false; self.nodes.len()];
        while let Some(current) = queue.pop_front() {
            ordered.push(&self.nodes[current]);
            placed[current] = true;
            for &next in &outgoing[current] {
                indegree[next] -= 1;
                if indegree[next] == 0 {
                    queue.push_back(next);
                }
            }
        }

        if ordered.len() != self.nodes.len() {
            return Err(GraphError::Cycle(self.cycle_members(&placed, &outgoing, &incoming)));
        }

        Ok(ordered)
    }

    /// The nodes not connected to the pipeline the first node belongs to.
    ///
    /// Connectivity is judged ignoring edge direction: a node feeding the
    /// pipeline and a node fed by it are both part of it, and a node joined to
    /// neither is not. A single-node graph is trivially connected and is
    /// rejected by the source and sink checks instead.
    ///
    /// The result is in declaration order, so an operator reads the ids in the
    /// order they wrote them.
    fn disconnected_nodes(
        &self,
        outgoing: &[Vec<usize>],
        incoming: &[Vec<usize>],
    ) -> Vec<NodeId> {
        if self.nodes.len() < 2 {
            return Vec::new();
        }

        let mut reached = vec![false; self.nodes.len()];
        reached[0] = true;
        let mut queue = VecDeque::from([0]);
        while let Some(current) = queue.pop_front() {
            for &next in outgoing[current].iter().chain(&incoming[current]) {
                if !std::mem::replace(&mut reached[next], true) {
                    queue.push_back(next);
                }
            }
        }

        (0..self.nodes.len())
            .filter(|&node| !reached[node])
            .map(|node| self.nodes[node].id.clone())
            .collect()
    }

    /// Narrows the nodes a topological sort could not place down to the ones
    /// actually on a cycle.
    ///
    /// Everything downstream of a cycle is unplaceable too, but naming those
    /// sends operators to the wrong node. Repeatedly stripping unplaced nodes
    /// that lead nowhere removes exactly those descendants and leaves the
    /// cycle itself.
    fn cycle_members(
        &self,
        placed: &[bool],
        outgoing: &[Vec<usize>],
        incoming: &[Vec<usize>],
    ) -> Vec<NodeId> {
        let mut removed = placed.to_vec();
        let mut remaining_out: Vec<usize> = outgoing
            .iter()
            .map(|targets| targets.iter().filter(|&&to| !removed[to]).count())
            .collect();

        let mut queue: VecDeque<usize> = (0..self.nodes.len())
            .filter(|&node| !removed[node] && remaining_out[node] == 0)
            .collect();
        while let Some(current) = queue.pop_front() {
            removed[current] = true;
            for &previous in &incoming[current] {
                if !removed[previous] {
                    remaining_out[previous] -= 1;
                    if remaining_out[previous] == 0 {
                        queue.push_back(previous);
                    }
                }
            }
        }

        (0..self.nodes.len())
            .filter(|&node| !removed[node])
            .map(|node| self.nodes[node].id.clone())
            .collect()
    }

    /// Maps node ids to their position, rejecting duplicates.
    fn index_nodes(&self) -> Result<HashMap<&str, usize>, GraphError> {
        let mut index = HashMap::with_capacity(self.nodes.len());
        for (position, node) in self.nodes.iter().enumerate() {
            if index.insert(node.id.as_str(), position).is_some() {
                return Err(GraphError::DuplicateNode(node.id.clone()));
            }
        }
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mic -> wake -> stt -> llm -> tts
    fn linear() -> PipelineGraph {
        PipelineGraph::new("linear")
            .with_node(Node::new("mic", NodeKind::Source, "websocket"))
            .with_node(Node::new("wake", NodeKind::WakeWord, "openwakeword"))
            .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
            .with_node(Node::new("llm", NodeKind::Llm, "ollama"))
            .with_node(Node::new("tts", NodeKind::Tts, "piper"))
            .with_edge(Edge::new("mic", "wake"))
            .with_edge(Edge::new("wake", "stt"))
            .with_edge(Edge::new("stt", "llm"))
            .with_edge(Edge::new("llm", "tts"))
    }

    #[test]
    fn linear_graph_orders_by_dependency() {
        let graph = linear();
        let order: Vec<&str> =
            graph.topological_order().expect("valid").iter().map(|n| n.id.as_str()).collect();
        assert_eq!(order, ["mic", "wake", "stt", "llm", "tts"]);
    }

    /// A router choosing between two models.
    ///
    /// This is a valid *graph* and not an executable *pipeline*: the runtime
    /// refuses both the router node and the second `llm`. The graph model is
    /// deliberately the wider of the two — a shape has to be expressible
    /// before it can be implemented — but the two must not disagree silently,
    /// so `crates/conduit-runtime/tests/plan.rs` asserts the refusal against
    /// this exact shape.
    fn router_fan_out() -> PipelineGraph {
        PipelineGraph::new("router")
            .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
            .with_node(Node::new("router", NodeKind::Router, "builtin"))
            .with_node(Node::new("local", NodeKind::Llm, "ollama"))
            .with_node(Node::new("cloud", NodeKind::Llm, "anthropic"))
            .with_node(Node::new("tts", NodeKind::Tts, "piper"))
            .with_edge(Edge::new("stt", "router"))
            .with_edge(Edge::from_port("router", "local", "local"))
            .with_edge(Edge::from_port("router", "cloud", "cloud"))
            .with_edge(Edge::new("local", "tts"))
            .with_edge(Edge::new("cloud", "tts"))
    }

    #[test]
    fn router_fan_out_joins_before_the_sink() {
        let graph = router_fan_out();
        let order: Vec<&str> =
            graph.topological_order().expect("valid").iter().map(|n| n.id.as_str()).collect();
        assert_eq!(order, ["stt", "router", "local", "cloud", "tts"]);
    }

    #[test]
    fn a_graph_with_no_edges_describes_no_pipeline() {
        // Every node has indegree 0, so `sources` is non-empty, and none has an
        // outgoing edge, so the sink check passes too. Without a connectivity
        // check this shape validated while saying nothing about order.
        let graph = PipelineGraph::new("unwired")
            .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
            .with_node(Node::new("llm", NodeKind::Llm, "ollama"))
            .with_node(Node::new("tts", NodeKind::Tts, "piper"));

        let Err(GraphError::Disconnected(nodes)) = graph.validate() else {
            panic!("an unwired graph must not validate");
        };
        assert_eq!(
            nodes,
            ["llm", "tts"],
            "named relative to the pipeline the first node is in"
        );
    }

    #[test]
    fn an_unreachable_node_is_rejected_rather_than_ignored() {
        // A node nothing feeds and that feeds nothing would never run, and a
        // graph that accepts it tells its author the opposite.
        let graph = linear().with_node(Node::new("orphan", NodeKind::Tool, "builtin"));

        assert_eq!(graph.validate(), Err(GraphError::Disconnected(vec!["orphan".to_owned()])));
    }

    #[test]
    fn two_separate_pipelines_in_one_graph_are_rejected() {
        let graph = PipelineGraph::new("two")
            .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
            .with_node(Node::new("llm", NodeKind::Llm, "ollama"))
            .with_node(Node::new("other-stt", NodeKind::Stt, "vosk"))
            .with_node(Node::new("other-llm", NodeKind::Llm, "vllm"))
            .with_edge(Edge::new("stt", "llm"))
            .with_edge(Edge::new("other-stt", "other-llm"));

        let Err(GraphError::Disconnected(nodes)) = graph.validate() else {
            panic!("two pipelines in one graph must not validate");
        };
        assert_eq!(nodes, ["other-stt", "other-llm"]);
    }

    #[test]
    fn a_node_that_only_feeds_the_pipeline_is_connected() {
        // Connectivity ignores direction: a tool hanging off the model is part
        // of the pipeline whichever way the edge points.
        linear()
            .with_node(Node::new("clock", NodeKind::Tool, "builtin"))
            .with_edge(Edge::new("clock", "llm"))
            .validate()
            .expect("an upstream branch is still one pipeline");
    }

    #[test]
    fn reachability_follows_edges_rather_than_declaration_order() {
        let graph = linear();
        assert!(graph.reaches("stt", "tts"), "transitively downstream");
        assert!(graph.reaches("mic", "stt"), "directly downstream");
        assert!(!graph.reaches("tts", "stt"), "edges are directed");
        assert!(!graph.reaches("stt", "stt"), "a node does not reach itself");
    }

    #[test]
    fn reachability_over_a_branch_covers_both_sides() {
        let graph = router_fan_out();
        assert!(graph.reaches("stt", "local"));
        assert!(graph.reaches("stt", "cloud"));
        assert!(graph.reaches("router", "tts"));
        assert!(!graph.reaches("local", "cloud"), "siblings do not reach each other");
    }

    #[test]
    fn reachability_of_an_unknown_node_is_false_rather_than_a_panic() {
        // The query is used on graphs a caller has not validated, so a bad id
        // must be an answer rather than a crash.
        let graph = linear();
        assert!(!graph.reaches("stt", "nope"));
        assert!(!graph.reaches("nope", "stt"));
    }

    #[test]
    fn reachability_terminates_on_a_cyclic_graph() {
        // Cyclic graphs do not validate, but `reaches` is callable on any
        // graph and must not spin.
        let graph = PipelineGraph::new("cyclic")
            .with_node(Node::new("a", NodeKind::Llm, "ollama"))
            .with_node(Node::new("b", NodeKind::Tool, "builtin"))
            .with_edge(Edge::new("a", "b"))
            .with_edge(Edge::new("b", "a"));

        assert!(graph.reaches("a", "b"));
        assert!(graph.reaches("a", "a"), "a cycle does lead back");
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let graph = PipelineGraph::new("dupe")
            .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
            .with_node(Node::new("stt", NodeKind::Stt, "vosk"));
        assert_eq!(graph.validate(), Err(GraphError::DuplicateNode("stt".into())));
    }

    #[test]
    fn dangling_edges_are_rejected() {
        let graph = PipelineGraph::new("dangling")
            .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
            .with_node(Node::new("tts", NodeKind::Tts, "piper"))
            .with_edge(Edge::new("stt", "nope"));
        assert_eq!(graph.validate(), Err(GraphError::UnknownNode("nope".into())));
    }

    #[test]
    fn cycles_are_rejected_and_name_the_participants() {
        let graph = PipelineGraph::new("cyclic")
            .with_node(Node::new("source", NodeKind::Source, "websocket"))
            .with_node(Node::new("a", NodeKind::Llm, "ollama"))
            .with_node(Node::new("b", NodeKind::Tool, "builtin"))
            .with_node(Node::new("sink", NodeKind::Sink, "websocket"))
            .with_edge(Edge::new("source", "a"))
            .with_edge(Edge::new("a", "b"))
            .with_edge(Edge::new("b", "a"))
            .with_edge(Edge::new("b", "sink"));

        let Err(GraphError::Cycle(nodes)) = graph.validate() else {
            panic!("expected a cycle");
        };
        assert_eq!(nodes, ["a", "b"]);
    }

    #[test]
    fn empty_graph_has_no_source() {
        assert_eq!(PipelineGraph::new("empty").validate(), Err(GraphError::NoSource));
    }

    #[test]
    fn graph_where_every_node_feeds_another_has_no_sink() {
        // A two-node cycle has no zero-indegree node either, so build a shape
        // that is only missing a terminal node: a self-contained ring is
        // caught by NoSource first, so use a node that points at itself plus
        // a reachable head.
        let graph = PipelineGraph::new("no sink")
            .with_node(Node::new("head", NodeKind::Source, "websocket"))
            .with_node(Node::new("loop", NodeKind::Llm, "ollama"))
            .with_edge(Edge::new("head", "loop"))
            .with_edge(Edge::new("loop", "loop"));
        assert_eq!(graph.validate(), Err(GraphError::NoSink));
    }

    #[test]
    fn graphs_survive_a_json_round_trip() {
        let graph = linear();
        let json = serde_json::to_string(&graph).expect("serialize");
        let decoded: PipelineGraph = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, graph);
        decoded.validate().expect("still valid");
    }
}
