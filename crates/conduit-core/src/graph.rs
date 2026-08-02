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
    /// Ingress from a device.
    Source,
    /// Wake word detection.
    WakeWord,
    /// Speech-to-text.
    Stt,
    /// Speaker identification.
    SpeakerId,
    /// A language model with its tool and memory bindings.
    Core,
    /// Text-to-speech.
    Tts,
    /// Egress to a device.
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
            Self::Core => "core",
            Self::Tts => "tts",
            Self::Sink => "sink",
        }
    }
}

/// What an edge carries.
///
/// A source and a sink declare theirs, because only their author knows whether
/// a pipeline is fed by a microphone or by a chat box. Every other kind derives
/// its own from what it does, so a miswired graph is a validation error rather
/// than a runtime surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    /// Sampled speech.
    Audio,
    /// Written words.
    Text,
    /// What a model said, before anything decided how to render it.
    ///
    /// Distinct from text so that a model stays unaware of how it will be
    /// heard: synthesis renders an utterance as speech and a text sink renders
    /// the same utterance as writing, and neither reopens the model.
    Utterance,
}

impl Modality {
    /// The word this modality is written as in a graph.
    ///
    /// The same spelling serde uses, so an error message and the JSON beside
    /// it describe an edge the same way.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Text => "text",
            Self::Utterance => "utterance",
        }
    }
}

impl std::fmt::Display for Modality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
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

impl MemoryMode {
    /// Whether this mode retrieves what was said before.
    #[must_use]
    pub const fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Whether this mode stores what is said now.
    #[must_use]
    pub const fn writes(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// Whether a tool may run without being asked about first.
///
/// A tool that changes something in the world is a different proposition from
/// one that answers a question, and the difference is not visible in the tool's
/// schema. The graph is where an operator says which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmPolicy {
    /// Dispatch as soon as the model asks for it.
    ///
    /// The default because it is what every tool does today, and a default of
    /// `always` would stall every existing pipeline on the first tool call.
    #[default]
    Never,
    /// Ask before dispatching, every time.
    Always,
}

/// Which language model a reasoning core reasons with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBinding {
    /// Provider definition id, e.g. `"ollama"`.
    pub provider: String,
    /// Model to request, or `None` for whichever model the provider definition
    /// serves first.
    ///
    /// Naming one here is what lets two pipelines share a provider definition
    /// and still reason with different models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A tool a reasoning core may call.
///
/// A binding is configuration on the core rather than an edge in the transport
/// pipeline: the model decides at runtime whether to call it, so there is no
/// static position for it to occupy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolBinding {
    /// Provider definition id, or a qualified tool id such as
    /// `"weather-tools.forecast"`.
    pub provider: String,
    /// Whether this pipeline wants to be asked before the tool runs.
    #[serde(default)]
    pub confirm: ConfirmPolicy,
}

/// A memory store a reasoning core retrieves from, writes to, or both.
///
/// Retrieval is an inflow and storage is an outflow on the same store, which is
/// why this is a binding and not two edges: a graph cannot say both without
/// describing a cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBinding {
    /// Provider definition id.
    pub provider: String,
    /// Whether this binding retrieves, stores, or both.
    #[serde(default)]
    pub mode: MemoryMode,
    /// Scope to confine retrieval and storage to, or `None` for all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// Most records to retrieve in one turn.
    #[serde(default = "default_memory_limit")]
    pub limit: usize,
}

/// A language model together with the tools and memory it may reach for.
///
/// The bindings have no execution order, because the model decides at runtime
/// what to call and how often. That is the whole reason they are configuration
/// on one node rather than stages in the transport pipeline: a topological
/// position for a tool would state an ordering that does not exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningCore {
    /// The model that does the reasoning.
    pub model: ModelBinding,
    /// System prompt for this pipeline, prepended to the definition's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Tools offered to the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolBinding>,
    /// Memory the model retrieves from and stores to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<MemoryBinding>,
    /// Cap on model calls in one turn.
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
}

impl ReasoningCore {
    /// A core reasoning with whichever model `provider` serves first, bound to
    /// no tools and no memory.
    ///
    /// Anything else is set on the returned value: the fields are the point of
    /// the type, and a constructor taking all five would read as five
    /// positional arguments at every call site that wants none of them.
    #[must_use]
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            model: ModelBinding { provider: provider.into(), model: None },
            system: None,
            tools: Vec::new(),
            memory: Vec::new(),
            max_rounds: DEFAULT_MAX_ROUNDS,
        }
    }
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
    /// Ingress from a device.
    Source {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"websocket"`.
        provider: String,
        /// What this source produces.
        ///
        /// Declared rather than derived: nothing about a websocket says
        /// whether it carries microphone samples or typed words.
        #[serde(default = "default_modality")]
        modality: Modality,
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
    /// A language model with its tool and memory bindings.
    Core {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// The model, and what it may reach for.
        core: ReasoningCore,
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
    /// Egress to a device.
    Sink {
        /// Author-chosen identifier, unique within the graph.
        id: NodeId,
        /// Provider definition id, e.g. `"websocket"`.
        provider: String,
        /// What this sink renders.
        ///
        /// Declared for the same reason a source declares one, and it is the
        /// declaration that decides how an utterance is delivered.
        #[serde(default = "default_modality")]
        modality: Modality,
    },
}

/// The default [`ReasoningCore::max_rounds`], as a serde default.
const fn default_max_rounds() -> usize {
    DEFAULT_MAX_ROUNDS
}

/// The modality of a source or sink that does not declare one.
///
/// Every graph written before modalities existed was a voice pipeline, so
/// audio is what those graphs meant. Reading them as anything else would
/// silently rewire pipelines the editor can still open and fix.
const fn default_modality() -> Modality {
    Modality::Audio
}

/// The default [`MemoryBinding::limit`], as a serde default.
const fn default_memory_limit() -> usize {
    DEFAULT_MEMORY_LIMIT
}

impl Node {
    /// Creates a `source` node producing `modality`.
    #[must_use]
    pub fn source(
        id: impl Into<NodeId>,
        provider: impl Into<String>,
        modality: Modality,
    ) -> Self {
        Self::Source { id: id.into(), provider: provider.into(), modality }
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

    /// Creates a `core` node reasoning with whichever model `provider` serves
    /// first, bound to no tools and no memory.
    ///
    /// A core with bindings is built through [`ReasoningCore`] and
    /// [`Node::Core`]: the bindings are the point of the type, and a
    /// constructor taking all of them would read as five positional arguments
    /// at every call site that wants none of them.
    #[must_use]
    pub fn core(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Core { id: id.into(), core: ReasoningCore::new(provider) }
    }

    /// Creates a `tts` node using the provider's default voice.
    #[must_use]
    pub fn tts(id: impl Into<NodeId>, provider: impl Into<String>) -> Self {
        Self::Tts { id: id.into(), provider: provider.into(), voice: None }
    }

    /// Creates a `sink` node rendering `modality`.
    #[must_use]
    pub fn sink(
        id: impl Into<NodeId>,
        provider: impl Into<String>,
        modality: Modality,
    ) -> Self {
        Self::Sink { id: id.into(), provider: provider.into(), modality }
    }

    /// This node's identifier within its graph.
    #[must_use]
    pub fn id(&self) -> &NodeId {
        match self {
            Self::Source { id, .. }
            | Self::WakeWord { id, .. }
            | Self::Stt { id, .. }
            | Self::SpeakerId { id, .. }
            | Self::Core { id, .. }
            | Self::Tts { id, .. }
            | Self::Sink { id, .. } => id,
        }
    }

    /// The provider definition this node selects.
    ///
    /// A `core` answers with the model it reasons with. Its tool and memory
    /// bindings name providers too, but they are attachments rather than the
    /// thing the node is, and a single answer is what every caller here wants.
    #[must_use]
    pub fn provider(&self) -> &str {
        match self {
            Self::Core { core, .. } => &core.model.provider,
            Self::Source { provider, .. }
            | Self::WakeWord { provider, .. }
            | Self::Stt { provider, .. }
            | Self::SpeakerId { provider, .. }
            | Self::Tts { provider, .. }
            | Self::Sink { provider, .. } => provider,
        }
    }

    /// Every provider definition id this node names.
    ///
    /// A core names one for its model and one per tool and memory binding, so
    /// anything asking "which providers does this pipeline depend on" — the
    /// delete refusal, provider validation — has to ask this rather than
    /// [`Node::provider`], which answers with the model alone.
    #[must_use]
    pub fn provider_references(&self) -> Vec<&str> {
        match self {
            Self::Core { core, .. } => std::iter::once(core.model.provider.as_str())
                .chain(core.tools.iter().map(|tool| tool.provider.as_str()))
                .chain(core.memory.iter().map(|store| store.provider.as_str()))
                .collect(),
            other => vec![other.provider()],
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
            Self::Core { .. } => NodeKind::Core,
            Self::Tts { .. } => NodeKind::Tts,
            Self::Sink { .. } => NodeKind::Sink,
        }
    }

    /// The word this node's kind is written as in a graph.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        self.kind().name()
    }

    /// What this node puts on its outgoing edges, or `None` when nothing does.
    ///
    /// Only a `sink` answers `None`: it terminates the pipeline, so there is
    /// nothing downstream for it to describe.
    #[must_use]
    pub const fn output_modality(&self) -> Option<Modality> {
        match self {
            Self::Source { modality, .. } => Some(*modality),
            Self::WakeWord { .. } | Self::SpeakerId { .. } | Self::Tts { .. } => {
                Some(Modality::Audio)
            }
            Self::Stt { .. } => Some(Modality::Text),
            Self::Core { .. } => Some(Modality::Utterance),
            Self::Sink { .. } => None,
        }
    }

    /// What this node can read from its incoming edges.
    ///
    /// Every kind answers, because every kind left in the graph is a stage in
    /// the transport pipeline. Tools and memory used to be the exception —
    /// each was the visible half of a call-and-return arc rather than a
    /// modality transform, so its edges went unchecked — and
    /// [ADR-0012](https://github.com/Teagan42/conduit/blob/main/docs/adr/0012-transport-pipeline-and-reasoning-core.md)
    /// moved both onto a reasoning core, where they have no edges at all.
    ///
    /// An empty slice is a node that reads nothing: a `source` originates its
    /// stream, so an edge into one delivers something nothing will consume.
    #[must_use]
    pub const fn accepted_modalities(&self) -> &'static [Modality] {
        match self {
            Self::Source { .. } => &[],
            Self::WakeWord { .. } | Self::SpeakerId { .. } | Self::Stt { .. } => {
                &[Modality::Audio]
            }
            Self::Core { .. } => &[Modality::Text],
            // Speech is one rendering of an utterance, and plain text is an
            // utterance nothing had to decide about, so synthesis speaks both.
            Self::Tts { .. } => &[Modality::Utterance, Modality::Text],
            Self::Sink { modality, .. } => match modality {
                Modality::Audio => &[Modality::Audio],
                // The other rendering of an utterance. A text sink writing one
                // down is what lets a model stay unaware of how it is
                // delivered, which is the whole reason an utterance is not
                // speech.
                Modality::Text => &[Modality::Text, Modality::Utterance],
                Modality::Utterance => &[Modality::Utterance],
            },
        }
    }

    /// Whether this node is where a model reasons.
    ///
    /// Asked rather than matched on, because validation counts reasoning nodes
    /// to refuse a pipeline with two models, and what that count means should
    /// not have to be re-derived at the one call site that needs it.
    #[must_use]
    pub const fn is_reasoning(&self) -> bool {
        matches!(self, Self::Core { .. })
    }
}

/// A directed connection between two nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Id of the upstream node.
    pub from: NodeId,
    /// Id of the downstream node.
    pub to: NodeId,
    /// Named output port on `from`, for a node with more than one output.
    /// `None` selects the node's default output, which is what every kind in
    /// the graph today has.
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
    /// is cyclic, it lacks an entry or exit point, it is not one connected
    /// pipeline, or an edge connects two nodes whose modalities do not line up.
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

        // Checked last, once the graph is known to be a pipeline at all.
        // Telling someone their microphone does not fit their model, when the
        // real problem is that two nodes point at each other, sends them to
        // the wrong node with the wrong question.
        //
        // A second model comes before the edge check for the same reason: what
        // its edges carry is a consequence of its being there, and the node to
        // delete is the answer either way.
        self.check_single_core()?;
        self.check_modalities()?;
        self.check_core_reachability()?;

        Ok(ordered)
    }

    /// Checks that the graph reasons in exactly one place, at most.
    ///
    /// Every reasoning node is named rather than only the surplus ones,
    /// because which of them the operator meant to keep is not something the
    /// graph knows.
    fn check_single_core(&self) -> Result<(), GraphError> {
        let reasoning: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|node| node.is_reasoning())
            .map(|node| node.id().clone())
            .collect();

        if reasoning.len() > 1 {
            return Err(GraphError::MultipleCores(reasoning));
        }
        Ok(())
    }

    /// Checks that every source feeds the core and the core feeds every sink.
    ///
    /// Modality typing is a property of one edge and cannot see this: a graph
    /// wiring `stt -> core` beside `stt -> tts` has two compatible edges and
    /// still drops the answer. Stating the rule needs a graph to have exactly
    /// one core, which is why it lives here now rather than in the runtime,
    /// which used to ask about each pair of stages it happened to know about.
    ///
    /// A graph with no core is not this check's business: resolution reports
    /// that, and complaining that nothing reaches a node the author never
    /// wrote would send them looking for the wrong problem.
    fn check_core_reachability(&self) -> Result<(), GraphError> {
        let Some(core) = self.nodes.iter().find(|node| node.is_reasoning()) else {
            return Ok(());
        };

        // Asked of where a node sits rather than of what kind it is. A
        // pipeline may end at a `tts` with no `sink` written down, and that
        // stage is still where the reply comes out.
        for node in &self.nodes {
            if node.id() == core.id() {
                continue;
            }
            let origin = !self.edges.iter().any(|edge| &edge.to == node.id());
            let terminal = !self.edges.iter().any(|edge| &edge.from == node.id());

            if origin && !self.reaches(node.id(), core.id()) {
                return Err(GraphError::SourceMissesCore(node.id().clone()));
            }
            if terminal && !self.reaches(core.id(), node.id()) {
                return Err(GraphError::SinkMissesCore(node.id().clone()));
            }
        }
        Ok(())
    }

    /// Checks that every edge delivers something its far end can read.
    ///
    /// The first mismatch is reported rather than all of them: a graph is
    /// usually miswired in one place, and a list of consequences reads as a
    /// worse problem than the one an operator has.
    fn check_modalities(&self) -> Result<(), GraphError> {
        for (position, edge) in self.edges.iter().enumerate() {
            let (Some(from), Some(to)) = (self.node(&edge.from), self.node(&edge.to)) else {
                continue;
            };
            let Some(produced) = from.output_modality() else {
                continue;
            };
            let expected = to.accepted_modalities();
            if !expected.contains(&produced) {
                return Err(GraphError::ModalityMismatch {
                    edge: position,
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    produced,
                    expected: expected.to_vec(),
                });
            }
        }
        Ok(())
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

    /// mic -> wake -> stt -> core -> tts
    fn linear() -> PipelineGraph {
        PipelineGraph::new("linear")
            .with_node(Node::source("mic", "websocket", Modality::Audio))
            .with_node(Node::wake_word("wake", "openwakeword"))
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::tts("tts", "piper"))
            .with_edge(Edge::new("mic", "wake"))
            .with_edge(Edge::new("wake", "stt"))
            .with_edge(Edge::new("stt", "core"))
            .with_edge(Edge::new("core", "tts"))
    }

    #[test]
    fn linear_graph_orders_by_dependency() {
        let graph = linear();
        let order: Vec<&str> =
            graph.topological_order().expect("valid").iter().map(|n| n.id().as_str()).collect();
        assert_eq!(order, ["mic", "wake", "stt", "core", "tts"]);
    }

    /// One transcript rendered by two synthesizers, rejoining at one sink.
    ///
    /// A valid *graph* rather than an executable *pipeline*: the runtime runs
    /// one synthesizer per turn. The graph model is deliberately the wider of
    /// the two — a shape has to be expressible before it can be implemented —
    /// and this is where the ordering of a fan-out that rejoins is pinned.
    fn fan_out() -> PipelineGraph {
        PipelineGraph::new("fan out")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::tts("local", "piper"))
            .with_node(Node::tts("cloud", "openai"))
            .with_node(Node::sink("speaker", "websocket", Modality::Audio))
            .with_edge(Edge::from_port("stt", "local", "local"))
            .with_edge(Edge::from_port("stt", "cloud", "cloud"))
            .with_edge(Edge::new("local", "speaker"))
            .with_edge(Edge::new("cloud", "speaker"))
    }

    #[test]
    fn a_fan_out_joins_before_the_sink() {
        let graph = fan_out();
        let order: Vec<&str> =
            graph.topological_order().expect("valid").iter().map(|n| n.id().as_str()).collect();
        assert_eq!(order, ["stt", "local", "cloud", "speaker"]);
    }

    #[test]
    fn a_graph_with_no_edges_describes_no_pipeline() {
        // Every node has indegree 0, so `sources` is non-empty, and none has an
        // outgoing edge, so the sink check passes too. Without a connectivity
        // check this shape validated while saying nothing about order.
        let graph = PipelineGraph::new("unwired")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::tts("tts", "piper"));

        let Err(GraphError::Disconnected(nodes)) = graph.validate() else {
            panic!("an unwired graph must not validate");
        };
        assert_eq!(
            nodes,
            ["core", "tts"],
            "named relative to the pipeline the first node is in"
        );
    }

    #[test]
    fn an_unreachable_node_is_rejected_rather_than_ignored() {
        // A node nothing feeds and that feeds nothing would never run, and a
        // graph that accepts it tells its author the opposite.
        let graph = linear().with_node(Node::stt("orphan", "vosk"));

        assert_eq!(graph.validate(), Err(GraphError::Disconnected(vec!["orphan".to_owned()])));
    }

    #[test]
    fn two_separate_pipelines_in_one_graph_are_rejected() {
        let graph = PipelineGraph::new("two")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::stt("other-stt", "vosk"))
            .with_node(Node::tts("other-tts", "piper"))
            .with_edge(Edge::new("stt", "core"))
            .with_edge(Edge::new("other-stt", "other-tts"));

        let Err(GraphError::Disconnected(nodes)) = graph.validate() else {
            panic!("two pipelines in one graph must not validate");
        };
        assert_eq!(nodes, ["other-stt", "other-tts"]);
    }

    #[test]
    fn a_node_that_only_feeds_the_pipeline_is_connected() {
        // Connectivity ignores direction: a second recognizer feeding the core
        // is part of the pipeline even though nothing in it feeds the
        // recognizer.
        linear()
            .with_node(Node::stt("aux", "vosk"))
            .with_edge(Edge::new("aux", "core"))
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
        let graph = fan_out();
        assert!(graph.reaches("stt", "local"));
        assert!(graph.reaches("stt", "cloud"));
        assert!(graph.reaches("stt", "speaker"), "transitively, over either branch");
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
            .with_node(Node::core("a", "ollama"))
            .with_node(Node::tts("b", "piper"))
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
            .with_node(Node::source("source", "websocket", Modality::Text))
            .with_node(Node::core("a", "ollama"))
            .with_node(Node::tts("b", "piper"))
            .with_node(Node::sink("sink", "websocket", Modality::Audio))
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
            .with_node(Node::source("head", "websocket", Modality::Text))
            .with_node(Node::core("loop", "ollama"))
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
        let node = Node::Tts {
            id: "tts".to_owned(),
            provider: "piper".to_owned(),
            voice: Some("alba".to_owned()),
        };

        assert_eq!(
            serde_json::to_value(&node).expect("serialize"),
            serde_json::json!({
                "kind": "tts",
                "id": "tts",
                "provider": "piper",
                "voice": "alba",
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
            serde_json::json!({ "kind": "tts", "id": "tts", "provider": "piper" }),
        )
        .expect("deserialize");

        assert_eq!(node, Node::tts("tts", "piper"));
        let Node::Tts { voice, .. } = node else {
            panic!("a `tts` tag deserializes to a `tts` node");
        };
        assert_eq!(voice, None, "no voice named means the provider's own");
    }

    #[test]
    fn a_setting_from_the_wrong_kind_of_node_is_rejected() {
        // Accepting `voice` on a recognizer would let an operator configure
        // something that could never take effect, and nothing would say so.
        let error = serde_json::from_value::<Node>(serde_json::json!({
            "kind": "stt",
            "id": "stt",
            "provider": "whisper",
            "voice": "alba",
        }))
        .expect_err("`voice` is not a recognition setting");

        assert!(error.to_string().contains("voice"), "{error}");
    }

    #[test]
    fn a_pipeline_wired_backwards_names_the_edge_that_cannot_carry_its_load() {
        // The defect modalities exist for: `tts -> core -> stt` used to be a
        // structurally perfect graph, and only the runtime's hand-written
        // expectation that recognition precedes reasoning caught it.
        let graph = PipelineGraph::new("backwards")
            .with_node(Node::tts("tts", "piper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::stt("stt", "whisper"))
            .with_edge(Edge::new("tts", "core"))
            .with_edge(Edge::new("core", "stt"));

        assert_eq!(
            graph.validate(),
            Err(GraphError::ModalityMismatch {
                edge: 0,
                from: "tts".to_owned(),
                to: "core".to_owned(),
                produced: Modality::Audio,
                expected: vec![Modality::Text],
            })
        );
    }

    #[test]
    fn a_modality_mismatch_reads_as_a_sentence_about_one_edge() {
        let graph = PipelineGraph::new("backwards")
            .with_node(Node::tts("tts", "piper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::stt("stt", "whisper"))
            .with_edge(Edge::new("tts", "core"))
            .with_edge(Edge::new("core", "stt"));

        assert_eq!(
            graph.validate().unwrap_err().to_string(),
            "edge `tts` -> `core` carries audio, but `core` accepts text"
        );
    }

    #[test]
    fn a_source_declares_what_it_produces_and_the_stage_after_it_must_read_it() {
        let graph = PipelineGraph::new("typed at a microphone")
            .with_node(Node::source("keyboard", "websocket", Modality::Text))
            .with_node(Node::stt("stt", "whisper"))
            .with_edge(Edge::new("keyboard", "stt"));

        let Err(GraphError::ModalityMismatch { produced, expected, .. }) = graph.validate()
        else {
            panic!("recognition has nothing to recognize in written words");
        };
        assert_eq!(produced, Modality::Text);
        assert_eq!(expected, [Modality::Audio]);
    }

    #[test]
    fn a_text_sink_renders_the_utterance_a_model_produced() {
        // Nothing has to stand between the model and a text sink, because text
        // is a rendering of an utterance rather than a different thing.
        PipelineGraph::new("text out")
            .with_node(Node::source("chat", "websocket", Modality::Text))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::sink("reply", "websocket", Modality::Text))
            .with_edge(Edge::new("chat", "core"))
            .with_edge(Edge::new("core", "reply"))
            .validate()
            .expect("a text sink writes down what the model said");
    }

    #[test]
    fn an_audio_sink_cannot_render_an_utterance_itself() {
        // Speech is a rendering an utterance needs a synthesizer for, so a
        // graph that skips synthesis is missing a stage rather than being
        // wired loosely.
        let graph = PipelineGraph::new("silent")
            .with_node(Node::source("mic", "websocket", Modality::Audio))
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::sink("speaker", "websocket", Modality::Audio))
            .with_edge(Edge::new("mic", "stt"))
            .with_edge(Edge::new("stt", "core"))
            .with_edge(Edge::new("core", "speaker"));

        let Err(GraphError::ModalityMismatch { from, to, produced, .. }) = graph.validate()
        else {
            panic!("an utterance is not audio until something speaks it");
        };
        assert_eq!((from.as_str(), to.as_str()), ("core", "speaker"));
        assert_eq!(produced, Modality::Utterance);
    }

    #[test]
    fn synthesis_speaks_both_an_utterance_and_plain_text() {
        for produced in [Node::core("upstream", "ollama"), Node::stt("upstream", "whisper")] {
            PipelineGraph::new("spoken")
                .with_node(produced)
                .with_node(Node::tts("tts", "piper"))
                .with_edge(Edge::new("upstream", "tts"))
                .validate()
                .expect("synthesis renders written words as readily as an utterance");
        }
    }

    #[test]
    fn an_edge_into_a_source_is_rejected_because_nothing_there_reads_it() {
        let graph = PipelineGraph::new("backwards at the edge")
            .with_node(Node::source("mic", "websocket", Modality::Audio))
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("core", "ollama"))
            .with_edge(Edge::new("stt", "mic"))
            .with_edge(Edge::new("stt", "core"));

        let error = graph.validate().unwrap_err();
        assert_eq!(
            error.to_string(),
            "edge `stt` -> `mic` carries text, but `mic` accepts nothing"
        );
    }

    #[test]
    fn a_graph_that_is_not_a_pipeline_is_described_as_such_before_its_modalities() {
        // This graph is wired backwards *and* cyclic. "These two nodes point at
        // each other" is the problem to fix; the modality complaint is a
        // consequence of it and names a different node.
        let graph = PipelineGraph::new("both wrong")
            .with_node(Node::source("mic", "websocket", Modality::Text))
            .with_node(Node::core("a", "ollama"))
            .with_node(Node::core("b", "ollama"))
            .with_node(Node::tts("tts", "piper"))
            .with_edge(Edge::new("mic", "a"))
            .with_edge(Edge::new("a", "b"))
            .with_edge(Edge::new("b", "a"))
            .with_edge(Edge::new("b", "tts"));

        assert_eq!(
            graph.validate(),
            Err(GraphError::Cycle(vec!["a".to_owned(), "b".to_owned()]))
        );
    }

    #[test]
    fn a_source_that_declares_no_modality_is_read_as_audio() {
        // Every graph written before modalities existed was a voice pipeline.
        // Refusing to load them, or loading them as something else, would
        // strand pipelines the editor can still open.
        let node: Node = serde_json::from_value(
            serde_json::json!({ "kind": "source", "id": "mic", "provider": "websocket" }),
        )
        .expect("deserialize");

        assert_eq!(node, Node::source("mic", "websocket", Modality::Audio));
    }

    #[test]
    fn a_declared_modality_is_always_written_down() {
        // Unlike the optional per-node settings, a modality is never absent
        // from what a pipeline saved: it decides what the graph means.
        assert_eq!(
            serde_json::to_value(Node::sink("reply", "websocket", Modality::Text))
                .expect("serialize"),
            serde_json::json!({
                "kind": "sink",
                "id": "reply",
                "provider": "websocket",
                "modality": "text",
            })
        );
    }

    #[test]
    fn a_modalitys_name_is_the_word_it_is_written_as_on_the_wire() {
        for modality in [Modality::Audio, Modality::Text, Modality::Utterance] {
            let written = serde_json::to_value(modality).expect("serialize");
            assert_eq!(written, serde_json::Value::String(modality.name().to_owned()));
            assert_eq!(modality.to_string(), modality.name());
        }
    }

    #[test]
    fn a_core_reads_words_and_produces_an_utterance() {
        // A core sits in the transport pipeline exactly where the model node
        // it replaced did: reading text and producing something nothing has
        // yet decided how to render. Its tools and memory are bindings and so
        // change nothing about the edges it can carry.
        let core = Node::core("core", "ollama");

        assert_eq!(core.output_modality(), Some(Modality::Utterance));
        assert_eq!(core.accepted_modalities(), &[Modality::Text][..]);

        PipelineGraph::new("cored")
            .with_node(Node::source("mic", "websocket", Modality::Audio))
            .with_node(Node::stt("stt", "whisper"))
            .with_node(core)
            .with_node(Node::tts("tts", "piper"))
            .with_edge(Edge::new("mic", "stt"))
            .with_edge(Edge::new("stt", "core"))
            .with_edge(Edge::new("core", "tts"))
            .validate()
            .expect("a core sits where a model node sat");
    }

    #[test]
    fn a_source_that_never_reaches_the_core_is_refused() {
        // Every edge here is modality-compatible — synthesis renders written
        // words as readily as an utterance — so no per-edge rule can see that
        // the model's answer is discarded and the transcript spoken instead.
        let sideways = PipelineGraph::new("sideways")
            .with_node(Node::source("mic", "websocket", Modality::Audio))
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::tts("tts", "piper"))
            .with_edge(Edge::new("mic", "stt"))
            .with_edge(Edge::new("stt", "core"))
            .with_edge(Edge::new("stt", "tts"));

        assert_eq!(sideways.validate(), Err(GraphError::SinkMissesCore("tts".to_owned())));
    }

    #[test]
    fn a_sink_the_core_never_feeds_is_refused() {
        let orphaned = PipelineGraph::new("orphan sink")
            .with_node(Node::source("chat", "websocket", Modality::Text))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::sink("out", "websocket", Modality::Text))
            .with_node(Node::sink("other", "websocket", Modality::Text))
            .with_edge(Edge::new("chat", "core"))
            .with_edge(Edge::new("core", "out"))
            .with_edge(Edge::new("chat", "other"));

        assert_eq!(orphaned.validate(), Err(GraphError::SinkMissesCore("other".to_owned())));
    }

    #[test]
    fn a_hybrid_pipeline_feeds_one_core_from_either_modality() {
        // The shape this track exists for: speak or type the question, hear
        // and read the answer, with one core in the middle that knows about
        // neither.
        PipelineGraph::new("hybrid")
            .with_node(Node::source("mic", "websocket", Modality::Audio))
            .with_node(Node::source("chat", "websocket", Modality::Text))
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("core", "ollama"))
            .with_node(Node::tts("tts", "piper"))
            .with_node(Node::sink("speaker", "websocket", Modality::Audio))
            .with_node(Node::sink("transcript", "websocket", Modality::Text))
            .with_edge(Edge::new("mic", "stt"))
            .with_edge(Edge::new("stt", "core"))
            .with_edge(Edge::new("chat", "core"))
            .with_edge(Edge::new("core", "tts"))
            .with_edge(Edge::new("tts", "speaker"))
            .with_edge(Edge::new("core", "transcript"))
            .validate()
            .expect("two ways in and two ways out is one pipeline");
    }

    #[test]
    fn two_reasoning_nodes_are_refused() {
        // A graph with two models says nothing about which answer is the
        // reply, so it is refused rather than resolved to whichever came
        // first.
        let graph = PipelineGraph::new("two minds")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("first", "ollama"))
            .with_node(Node::core("second", "anthropic"))
            .with_node(Node::tts("tts", "piper"))
            .with_edge(Edge::new("stt", "first"))
            .with_edge(Edge::new("stt", "second"))
            .with_edge(Edge::new("first", "tts"))
            .with_edge(Edge::new("second", "tts"));

        assert_eq!(
            graph.validate(),
            Err(GraphError::MultipleCores(vec!["first".to_owned(), "second".to_owned()]))
        );
    }

    #[test]
    fn two_reasoning_nodes_read_as_a_sentence_naming_both() {
        let graph = PipelineGraph::new("two minds")
            .with_node(Node::stt("stt", "whisper"))
            .with_node(Node::core("local", "ollama"))
            .with_node(Node::core("cloud", "anthropic"))
            .with_edge(Edge::new("stt", "local"))
            .with_edge(Edge::new("stt", "cloud"));

        assert_eq!(
            graph.validate().unwrap_err().to_string(),
            "graph has more than one reasoning node: local, cloud"
        );
    }

    #[test]
    fn a_core_carries_its_tools_and_memory_rather_than_wiring_them() {
        let core = ReasoningCore {
            model: ModelBinding {
                provider: "ollama".to_owned(),
                model: Some("qwen3:8b".to_owned()),
            },
            system: Some("Be brief.".to_owned()),
            tools: vec![ToolBinding {
                provider: "lights".to_owned(),
                confirm: ConfirmPolicy::Always,
            }],
            memory: vec![MemoryBinding {
                provider: "recall".to_owned(),
                mode: MemoryMode::ReadWrite,
                scope: Some(Scope::Speaker),
                limit: 3,
            }],
            max_rounds: 2,
        };

        assert_eq!(
            serde_json::to_value(Node::Core { id: "core".to_owned(), core })
                .expect("serialize"),
            serde_json::json!({
                "kind": "core",
                "id": "core",
                "core": {
                    "model": { "provider": "ollama", "model": "qwen3:8b" },
                    "system": "Be brief.",
                    "tools": [{ "provider": "lights", "confirm": "always" }],
                    "memory": [{
                        "provider": "recall",
                        "mode": "read_write",
                        "scope": "speaker",
                        "limit": 3,
                    }],
                    "max_rounds": 2,
                },
            })
        );
    }

    #[test]
    fn a_core_with_nothing_bound_to_it_is_written_as_just_its_model() {
        // An operator reading a saved pipeline should see what it decided, so
        // a core with no tools is silent about tools rather than listing none.
        assert_eq!(
            serde_json::to_value(Node::core("core", "ollama")).expect("serialize"),
            serde_json::json!({
                "kind": "core",
                "id": "core",
                "core": { "model": { "provider": "ollama" }, "max_rounds": DEFAULT_MAX_ROUNDS },
            })
        );
    }

    #[test]
    fn omitted_core_settings_fall_back_to_the_documented_defaults() {
        let node: Node = serde_json::from_value(serde_json::json!({
            "kind": "core",
            "id": "core",
            "core": {
                "model": { "provider": "ollama" },
                "tools": [{ "provider": "lights" }],
                "memory": [{ "provider": "recall" }],
            },
        }))
        .expect("deserialize");

        let Node::Core { core, .. } = node else {
            panic!("a `core` tag deserializes to a `core` node");
        };
        assert_eq!(core.model.model, None, "no model named means the provider's first");
        assert_eq!(core.max_rounds, DEFAULT_MAX_ROUNDS);
        assert_eq!(
            core.tools[0].confirm,
            ConfirmPolicy::Never,
            "a tool nobody said to ask about runs when the model asks"
        );
        assert_eq!(core.memory[0].mode, MemoryMode::Read);
        assert_eq!(core.memory[0].scope, None, "unset means every scope");
        assert_eq!(core.memory[0].limit, DEFAULT_MEMORY_LIMIT);
    }

    #[test]
    fn a_setting_from_the_wrong_kind_of_binding_is_rejected() {
        // The same rule the node variants follow: a setting that could never
        // take effect is a mistake nothing else would report.
        let error = serde_json::from_value::<Node>(serde_json::json!({
            "kind": "core",
            "id": "core",
            "core": {
                "model": { "provider": "ollama" },
                "tools": [{ "provider": "lights", "limit": 3 }],
            },
        }))
        .expect_err("`limit` is a memory setting, not a tool one");

        assert!(error.to_string().contains("limit"), "{error}");
    }

    #[test]
    fn a_kinds_name_is_the_word_it_is_written_as_on_the_wire() {
        // Error messages name a node kind and so does the JSON beside them;
        // two spellings of `wake_word` would send an operator looking for a
        // node that is not there.
        for node in [
            Node::source("a", "p", Modality::Audio),
            Node::wake_word("b", "p"),
            Node::stt("c", "p"),
            Node::speaker_id("d", "p"),
            Node::core("e", "p"),
            Node::tts("i", "p"),
            Node::sink("j", "p", Modality::Audio),
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
