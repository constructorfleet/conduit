//! The pipeline graph: a validated, serializable description of one voice
//! pipeline.
//!
//! A graph is data, not code. The web UI edits it, the API stores it, and the
//! runtime executes it — none of which requires recompiling Conduit or
//! knowing which providers a node refers to.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::error::GraphError;
use crate::memory::Scope;

/// A node's identifier within its graph. Unique per graph, chosen by the
/// author rather than generated, so graphs stay diffable.
pub type NodeId = String;

/// How many times a model may be called in one turn before the runtime stops.
///
/// A model that keeps requesting tools would otherwise loop forever while the
/// person who asked the question waits.
pub const DEFAULT_MAX_ROUNDS: usize = 4;

/// How many records a memory node retrieves when the graph does not say.
///
/// Retrieved records are spent as prompt context, so the default is small
/// enough that turning memory on cannot quietly crowd out the conversation.
pub const DEFAULT_MEMORY_LIMIT: usize = 5;

/// What a node does in the pipeline.
///
/// The kind determines the shape of a node's inputs and outputs; the node's
/// provider determines who implements it. This is the discriminant of [`Node`],
/// which is where a node's configuration lives.
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

impl NodeKind {
    /// The word this kind is written as in a graph.
    ///
    /// The same spelling serde uses, so an error message and the JSON an
    /// operator is looking at name the node the same way.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::WakeWord => "wake_word",
            Self::Stt => "stt",
            Self::SpeakerId => "speaker_id",
            Self::Router => "router",
            Self::Llm => "llm",
            Self::Tool => "tool",
            Self::Memory => "memory",
            Self::Tts => "tts",
            Self::Sink => "sink",
        }
    }
}

/// What a memory node does with the conversation it is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    /// Retrieve before the model runs, store nothing.
    ///
    /// The default because it is the only mode that cannot surprise an
    /// operator by writing down what was said.
    #[default]
    Read,
    /// Store after the model runs, retrieve nothing.
    Write,
    /// Retrieve before and store after.
    ReadWrite,
}

/// One stage in a pipeline, with the settings that belong to it here.
///
/// A node names a provider *definition* by id, and definitions are shared: two
/// pipelines can point at one language model provider and still request
/// different models, because which model to request is a property of this
/// pipeline rather than of the definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /// Audio ingress from a device.
    Source {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"websocket"`.
        provider: String,
    },
    /// Wake word detection.
    WakeWord {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"openwakeword"`.
        provider: String,
    },
    /// Speech-to-text.
    Stt {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"whisper"`.
        provider: String,
    },
    /// Speaker identification.
    SpeakerId {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id.
        provider: String,
    },
    /// Conditional fan-out to one of several downstream branches.
    Router {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id.
        provider: String,
    },
    /// Language model inference.
    Llm {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"ollama"`.
        provider: String,
        /// Model to request, or `None` for whichever model the provider
        /// definition serves first.
        ///
        /// Naming one here is what lets two pipelines share a provider
        /// definition and still reason with different models.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// System prompt for this pipeline, prepended to the definition's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<String>,
        /// Cap on model calls in one turn.
        #[serde(default = "default_max_rounds")]
        max_rounds: usize,
    },
    /// Tool execution.
    Tool {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, or a qualified tool id such as
        /// `"weather-tools.forecast"`.
        provider: String,
    },
    /// Memory read/write.
    Memory {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id.
        provider: String,
        /// Whether this node retrieves, stores, or both.
        #[serde(default)]
        mode: MemoryMode,
        /// Scope to confine retrieval and storage to, or `None` for all of
        /// them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<Scope>,
        /// Most records to retrieve in one turn.
        #[serde(default = "default_memory_limit")]
        limit: usize,
    },
    /// Text-to-speech.
    Tts {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"piper"`.
        provider: String,
        /// Voice to request, or `None` for the provider's own default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
    },
    /// Audio egress to a device.
    Sink {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"websocket"`.
        provider: String,
    },
}

/// The default [`Node::Llm::max_rounds`], as a serde default.
const fn default_max_rounds() -> usize {
    DEFAULT_MAX_ROUNDS
}

/// The default [`Node::Memory::limit`], as a serde default.
const fn default_memory_limit() -> usize {
    DEFAULT_MEMORY_LIMIT
}

impl Node {
    /// Creates a `source` node.
    #[must_use]
    pub fn source(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Source { id: id.into(), provider: provider.into() }
    }

    /// Creates a `wake_word` node.
    #[must_use]
    pub fn wake_word(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::WakeWord { id: id.into(), provider: provider.into() }
    }

    /// Creates an `stt` node.
    #[must_use]
    pub fn stt(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Stt { id: id.into(), provider: provider.into() }
    }

    /// Creates a `speaker_id` node.
    #[must_use]
    pub fn speaker_id(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::SpeakerId { id: id.into(), provider: provider.into() }
    }

    /// Creates a `router` node.
    #[must_use]
    pub fn router(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Router { id: id.into(), provider: provider.into() }
    }

    /// Creates an `llm` node configured as the provider definition sees fit.
    ///
    /// A pipeline that wants a particular model, prompt, or round cap builds
    /// [`Node::Llm`] directly; those fields are the point of the variant, and
    /// a constructor taking all of them would read as five positional
    /// arguments at every call site that wants none of them.
    #[must_use]
    pub fn llm(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Llm {
            id: id.into(),
            provider: provider.into(),
            model: None,
            system: None,
            max_rounds: DEFAULT_MAX_ROUNDS,
        }
    }

    /// Creates a `tool` node.
    #[must_use]
    pub fn tool(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Tool { id: id.into(), provider: provider.into() }
    }

    /// Creates a `memory` node that retrieves from every scope.
    #[must_use]
    pub fn memory(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Memory {
            id: id.into(),
            provider: provider.into(),
            mode: MemoryMode::Read,
            scope: None,
            limit: DEFAULT_MEMORY_LIMIT,
        }
    }

    /// Creates a `tts` node using the provider's default voice.
    #[must_use]
    pub fn tts(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Tts { id: id.into(), provider: provider.into(), voice: None }
    }

    /// Creates a `sink` node.
    #[must_use]
    pub fn sink(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Sink { id: id.into(), provider: provider.into() }
    }

    /// This node's identifier within its graph.
    #[must_use]
    pub fn id(&self) -> &NodeId {
        match self {
            Self::Source { id, .. }
            | Self::WakeWord { id, .. }
            | Self::Stt { id, .. }
            | Self::SpeakerId { id, .. }
            | Self::Router { id, .. }
            | Self::Llm { id, .. }
            | Self::Tool { id, .. }
            | Self::Memory { id, .. }
            | Self::Tts { id, .. }
            | Self::Sink { id, .. } => id,
        }
    }

    /// The provider definition this node selects.
    #[must_use]
    pub fn provider(&self) -> &str {
        match self {
            Self::Source { provider, .. }
            | Self::WakeWord { provider, .. }
            | Self::Stt { provider, .. }
            | Self::SpeakerId { provider, .. }
            | Self::Router { provider, .. }
            | Self::Llm { provider, .. }
            | Self::Tool { provider, .. }
            | Self::Memory { provider, .. }
            | Self::Tts { provider, .. }
            | Self::Sink { provider, .. } => provider,
        }
    }

    /// What this node does.
    #[must_use]
    pub const fn kind(&self) -> NodeKind {
        match self {
            Self::Source { .. } => NodeKind::Source,
            Self::WakeWord { .. } => NodeKind::WakeWord,
            Self::Stt { .. } => NodeKind::Stt,
            Self::SpeakerId { .. } => NodeKind::SpeakerId,
            Self::Router { .. } => NodeKind::Router,
            Self::Llm { .. } => NodeKind::Llm,
            Self::Tool { .. } => NodeKind::Tool,
            Self::Memory { .. } => NodeKind::Memory,
            Self::Tts { .. } => NodeKind::Tts,
            Self::Sink { .. } => NodeKind::Sink,
        }
    }

    /// The word this node's kind is written as in a graph.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        self.kind().name()
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
        self.nodes.iter().find(|node| node.id() == id)
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
            .map(|node| self.nodes[node].id().clone())
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
            .map(|node| self.nodes[node].id().clone())
            .collect()
    }

    /// Maps node ids to their position, rejecting duplicates.
    fn index_nodes(&self) -> Result<HashMap<&str, usize>, GraphError> {
        let mut index = HashMap::with_capacity(self.nodes.len());
        for (position, node) in self.nodes.iter().enumerate() {
            if index.insert(node.id().as_str(), position).is_some() {
                return Err(GraphError::DuplicateNode(node.id().clone()));
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
            .with_node(Node::source("mic", "websocket"))
            .with_node(Node::wake_word("wake", "openwakeword"))
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::llm("llm", "ollama"))
            .with_node(Node::tts("tts", "piper"))
            .with_edge(Edge::new("mic", "wake"))
            .with_edge(Edge::new("wake", "stt"))
            .with_edge(Edge::new("stt", "llm"))
            .with_edge(Edge::new("llm", "tts"))
    }

    #[test]
    fn linear_graph_orders_by_dependency() {
        let graph = linear();
        let order: Vec<&str> =
            graph.topological_order().expect("valid").iter().map(|n| n.id().as_str()).collect();
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
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::router("router", "builtin"))
            .with_node(Node::llm("local", "ollama"))
            .with_node(Node::llm("cloud", "anthropic"))
            .with_node(Node::tts("tts", "piper"))
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
            graph.topological_order().expect("valid").iter().map(|n| n.id().as_str()).collect();
        assert_eq!(order, ["stt", "router", "local", "cloud", "tts"]);
    }

    #[test]
    fn a_graph_with_no_edges_describes_no_pipeline() {
        // Every node has indegree 0, so `sources` is non-empty, and none has an
        // outgoing edge, so the sink check passes too. Without a connectivity
        // check this shape validated while saying nothing about order.
        let graph = PipelineGraph::new("unwired")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::llm("llm", "ollama"))
            .with_node(Node::tts("tts", "piper"));

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
        let graph = linear().with_node(Node::tool("orphan", "builtin"));

        assert_eq!(graph.validate(), Err(GraphError::Disconnected(vec!["orphan".to_owned()])));
    }

    #[test]
    fn two_separate_pipelines_in_one_graph_are_rejected() {
        let graph = PipelineGraph::new("two")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::llm("llm", "ollama"))
            .with_node(Node::stt("other-stt", "vosk"))
            .with_node(Node::llm("other-llm", "vllm"))
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
            .with_node(Node::tool("clock", "builtin"))
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
            .with_node(Node::llm("a", "ollama"))
            .with_node(Node::tool("b", "builtin"))
            .with_edge(Edge::new("a", "b"))
            .with_edge(Edge::new("b", "a"));

        assert!(graph.reaches("a", "b"));
        assert!(graph.reaches("a", "a"), "a cycle does lead back");
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let graph = PipelineGraph::new("dupe")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::stt("stt", "vosk"));
        assert_eq!(graph.validate(), Err(GraphError::DuplicateNode("stt".into())));
    }

    #[test]
    fn dangling_edges_are_rejected() {
        let graph = PipelineGraph::new("dangling")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::tts("tts", "piper"))
            .with_edge(Edge::new("stt", "nope"));
        assert_eq!(graph.validate(), Err(GraphError::UnknownNode("nope".into())));
    }

    #[test]
    fn cycles_are_rejected_and_name_the_participants() {
        let graph = PipelineGraph::new("cyclic")
            .with_node(Node::source("source", "websocket"))
            .with_node(Node::llm("a", "ollama"))
            .with_node(Node::tool("b", "builtin"))
            .with_node(Node::sink("sink", "websocket"))
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
            .with_node(Node::source("head", "websocket"))
            .with_node(Node::llm("loop", "ollama"))
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

    #[test]
    fn a_node_is_tagged_by_its_kind_and_carries_its_own_settings() {
        // The tag is what makes a node's configuration readable: `kind` says
        // which fields the rest of the object is allowed to have.
        let node = Node::Llm {
            id: "llm".to_owned(),
            provider: "ollama".to_owned(),
            model: Some("qwen3:8b".to_owned()),
            system: Some("Be brief.".to_owned()),
            max_rounds: 2,
        };

        assert_eq!(
            serde_json::to_value(&node).expect("serialize"),
            serde_json::json!({
                "kind": "llm",
                "id": "llm",
                "provider": "ollama",
                "model": "qwen3:8b",
                "system": "Be brief.",
                "max_rounds": 2,
            })
        );
    }

    #[test]
    fn unset_node_settings_are_left_out_rather_than_written_as_null() {
        // A graph an operator reads should only mention what that pipeline
        // decided, so an unnamed model is absent rather than an explicit null.
        assert_eq!(
            serde_json::to_value(Node::tts("tts", "piper")).expect("serialize"),
            serde_json::json!({ "kind": "tts", "id": "tts", "provider": "piper" })
        );
    }

    #[test]
    fn omitted_node_settings_fall_back_to_the_documented_defaults() {
        let node: Node = serde_json::from_value(
            serde_json::json!({ "kind": "llm", "id": "llm", "provider": "ollama" }),
        )
        .expect("deserialize");

        assert_eq!(node, Node::llm("llm", "ollama"));
        let Node::Llm { model, system, max_rounds, .. } = node else {
            panic!("an `llm` tag deserializes to an `llm` node");
        };
        assert_eq!(model, None, "no model named means the provider's first");
        assert_eq!(system, None);
        assert_eq!(max_rounds, DEFAULT_MAX_ROUNDS);
    }

    #[test]
    fn a_setting_from_the_wrong_kind_of_node_is_rejected() {
        // Accepting `voice` on a model node would let an operator configure
        // something that could never take effect, and nothing would say so.
        let error = serde_json::from_value::<Node>(serde_json::json!({
            "kind": "llm",
            "id": "llm",
            "provider": "ollama",
            "voice": "alba",
        }))
        .expect_err("`voice` is not a language model setting");

        assert!(error.to_string().contains("voice"), "{error}");
    }

    #[test]
    fn a_memory_node_defaults_to_retrieving_from_every_scope() {
        let Node::Memory { mode, scope, limit, .. } = Node::memory("memory", "builtin") else {
            panic!("the constructor builds a memory node");
        };

        assert_eq!(mode, MemoryMode::Read);
        assert_eq!(scope, None, "unset means every scope, as in a memory query");
        assert_eq!(limit, DEFAULT_MEMORY_LIMIT);
    }

    #[test]
    fn a_kinds_name_is_the_word_it_is_written_as_on_the_wire() {
        // Error messages name a node kind and so does the JSON beside them;
        // two spellings of `wake_word` would send an operator looking for a
        // node that is not there.
        for node in [
            Node::source("a", "p"),
            Node::wake_word("b", "p"),
            Node::stt("c", "p"),
            Node::speaker_id("d", "p"),
            Node::router("e", "p"),
            Node::llm("f", "p"),
            Node::tool("g", "p"),
            Node::memory("h", "p"),
            Node::tts("i", "p"),
            Node::sink("j", "p"),
        ] {
            let tag = serde_json::to_value(&node).expect("serialize")["kind"]
                .as_str()
                .expect("the tag is a string")
                .to_owned();
            assert_eq!(node.kind_name(), tag);
            assert_eq!(node.kind().name(), tag);
        }
    }
}
