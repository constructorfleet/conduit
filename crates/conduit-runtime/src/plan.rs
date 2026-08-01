//! Turning a validated graph into the concrete providers that will run it.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::graph::{Node, NodeKind, PipelineGraph};
use conduit_core::{Error, Result};
use conduit_provider::llm::{LanguageModel, ToolSpec};
use conduit_provider::stt::SpeechToText;
use conduit_provider::tool::Tool;
use conduit_provider::tts::TextToSpeech;

use crate::Providers;

/// How many times a model may be called in one turn before the runtime stops.
///
/// A model that keeps requesting tools would otherwise loop forever while the
/// person who asked the question waits.
const DEFAULT_MAX_TOOL_ROUNDS: usize = 4;

/// The resolved providers and settings for one pipeline.
///
/// Resolution happens once, at prepare time, so a turn never pays for a
/// registry lookup and a misconfigured graph fails before any audio arrives.
pub struct Plan {
    /// Pipeline this plan executes.
    pub pipeline: String,
    /// Recognizer, and the node that selected it.
    pub stt: Arc<dyn SpeechToText>,
    /// Node id of the recognizer, used when reporting failures.
    pub stt_node: String,
    /// Language model, and the node that selected it.
    pub llm: Arc<dyn LanguageModel>,
    /// Node id of the model.
    pub llm_node: String,
    /// Model identifier to request.
    pub model: String,
    /// System prompt attached by provider registry configuration, when present.
    pub system: Option<String>,
    /// Synthesizer.
    pub tts: Arc<dyn TextToSpeech>,
    /// Node id of the synthesizer.
    pub tts_node: String,
    /// Voice to request from provider registry configuration, when present.
    pub voice: Option<String>,
    /// Tools offered to the model, keyed by the name it calls them by.
    ///
    /// Unlike the other stages a pipeline may have any number of these, so
    /// tool nodes are collected rather than treated as one slot.
    pub tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Cap on model calls in one turn.
    pub max_tool_rounds: usize,
}

impl Plan {
    /// The tool schemas to advertise to the model.
    #[must_use]
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    /// Resolves `graph` against `providers`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGraph`] if the graph is not executable at all,
    /// [`Error::UnknownProvider`] if a node names a provider that is not
    /// registered, and [`Error::Config`] if the topology is one the runtime
    /// cannot execute yet.
    pub fn resolve(graph: &PipelineGraph, providers: &Providers) -> Result<Self> {
        let mut stt = None;
        let mut llm = None;
        let mut tts = None;
        let mut tools = BTreeMap::new();
        let mut tool_nodes = Vec::new();

        for node in graph.topological_order()? {
            match node.kind {
                // Endpoints describe where audio enters and leaves; the
                // caller supplies both, so there is nothing to resolve.
                NodeKind::Source | NodeKind::Sink => {}
                NodeKind::Stt => {
                    reject_duplicate(&stt, node)?;
                    stt = Some((providers.stt().require(&node.provider)?, node.id.clone()));
                }
                NodeKind::Llm => {
                    reject_duplicate(&llm, node)?;
                    llm = Some((
                        providers.llm().require(&node.provider)?,
                        node.id.clone(),
                        node.provider.clone(),
                        None,
                        DEFAULT_MAX_TOOL_ROUNDS,
                    ));
                }
                NodeKind::Tool => {
                    let tool = providers.tools().require(&node.provider)?;
                    let name = tool.spec().name;
                    if tools.insert(name.clone(), tool).is_some() {
                        return Err(Error::Config(format!(
                            "two tools are both called `{name}`; the model could not \
                             tell them apart (node `{}`)",
                            node.id
                        )));
                    }
                    tool_nodes.push(node.id.clone());
                }
                NodeKind::Tts => {
                    reject_duplicate(&tts, node)?;
                    let provider = providers.tts().require(&node.provider)?;
                    tts = Some((provider, node.id.clone(), None));
                }
                // Explicitly refused rather than skipped. A router that is
                // accepted and then ignored turns "send hard questions to the
                // cloud model" into "send everything to whichever model
                // resolved", which is worse than refusing to run the graph.
                NodeKind::Router => {
                    return Err(Error::Config(format!(
                        "`router` nodes are not executable yet, and running this graph \
                         would ignore the routing it describes (node `{}`)",
                        node.id
                    )))
                }
                kind => {
                    return Err(Error::Config(format!(
                        "`{}` nodes are not executable yet (node `{}`)",
                        kind_name(kind),
                        node.id
                    )))
                }
            }
        }

        let (stt, stt_node) = stt.ok_or_else(|| missing("stt"))?;
        let (llm, llm_node, model, system, max_tool_rounds) =
            llm.ok_or_else(|| missing("llm"))?;
        let (tts, tts_node, voice) = tts.ok_or_else(|| missing("tts"))?;

        // This runtime executes recognition, then reasoning, then synthesis,
        // in that order. A graph is only a description of *this* pipeline if
        // its edges say the same thing — otherwise a graph wired
        // `tts -> llm -> stt` would run identically to a correct one, and its
        // author would have no way to find out.
        require_downstream(graph, &stt_node, &llm_node)?;
        require_downstream(graph, &llm_node, &tts_node)?;
        for tool_node in &tool_nodes {
            require_downstream(graph, &llm_node, tool_node)?;
        }

        if !tools.is_empty() && !llm.supports_tools() {
            return Err(Error::Config(format!(
                "node `{llm_node}` uses provider `{}`, which cannot call tools, but the \
                 pipeline defines {} of them",
                llm.name(),
                tools.len()
            )));
        }
        let models = llm.models();
        if !models.is_empty() && !models.iter().any(|served| served == &model) {
            return Err(Error::Config(format!(
                "node `{llm_node}` requests model `{model}`, but provider `{}` only serves: {}",
                llm.name(),
                models.join(", ")
            )));
        }

        Ok(Self {
            pipeline: graph.name.clone(),
            stt,
            stt_node,
            llm,
            llm_node,
            model,
            system,
            tts,
            tts_node,
            voice,
            tools,
            max_tool_rounds,
        })
    }
}

/// Rejects a second node of a kind the runtime can only run once.
///
/// Tool branches can fan out, but capture, reasoning, and synthesis are still
/// single-stage contracts in one turn.
fn reject_duplicate<T>(existing: &Option<T>, node: &Node) -> Result<()> {
    if existing.is_some() {
        return Err(Error::Config(format!(
            "more than one `{}` node; this runtime executes only one per turn \
             (node `{}`)",
            kind_name(node.kind),
            node.id
        )));
    }
    Ok(())
}

/// Requires that `downstream` is reachable from `upstream`.
///
/// The check is reachability rather than a direct edge, so a graph may put a
/// wake word, speaker id, or router node between two stages once those are
/// executable, and an existing graph does not have to be rewired to keep
/// working.
fn require_downstream(graph: &PipelineGraph, upstream: &str, downstream: &str) -> Result<()> {
    if graph.reaches(upstream, downstream) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "node `{downstream}` is not downstream of `{upstream}`, but this runtime would \
         run it as though it were; add the edges the pipeline needs"
    )))
}

/// The name a node kind is written as in a graph.
fn kind_name(kind: NodeKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Error for a pipeline missing a stage the runtime requires.
fn missing(kind: &str) -> Error {
    Error::Config(format!("pipeline has no `{kind}` node"))
}

/// Written by hand because a plan holds trait objects, which are not `Debug`.
/// Shows the resolved wiring, which is what anyone printing a plan wants.
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("stt", &format_args!("{} ({})", self.stt_node, self.stt.name()))
            .field("llm", &format_args!("{} ({})", self.llm_node, self.llm.name()))
            .field("model", &self.model)
            .field("system", &self.system)
            .field("tts", &format_args!("{} ({})", self.tts_node, self.tts.name()))
            .field("voice", &self.voice)
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}
