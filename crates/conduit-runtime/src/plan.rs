//! Turning a validated graph into the concrete providers that will run it.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::graph::{Node, PipelineGraph};
use conduit_core::{Error, Result};
use conduit_provider::llm::{LanguageModel, ToolSpec};
use conduit_provider::stt::SpeechToText;
use conduit_provider::tool::Tool;
use conduit_provider::tts::TextToSpeech;

use crate::Providers;

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
    /// System prompt this pipeline's model node asks for, when present.
    pub system: Option<String>,
    /// Synthesizer.
    pub tts: Arc<dyn TextToSpeech>,
    /// Node id of the synthesizer.
    pub tts_node: String,
    /// Voice this pipeline's synthesis node asks for, when present.
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
            match node {
                // Endpoints describe where audio enters and leaves; the
                // caller supplies both, so there is nothing to resolve.
                Node::Source { .. } | Node::Sink { .. } => {}
                Node::Stt { id, provider } => {
                    reject_duplicate(&stt, node)?;
                    stt = Some((providers.stt().require(provider)?, id.clone()));
                }
                Node::Llm { id, provider, model, system, max_rounds } => {
                    reject_duplicate(&llm, node)?;
                    let language_model = providers.llm().require(provider)?;
                    let model = resolve_model(language_model.as_ref(), node, model.as_deref())?;
                    llm =
                        Some((language_model, id.clone(), model, system.clone(), *max_rounds));
                }
                Node::Tool { id, provider } => {
                    let tool = providers.tools().require(provider)?;
                    let name = tool.spec().name;
                    if tools.insert(name.clone(), tool).is_some() {
                        return Err(Error::Config(format!(
                            "two tools are both called `{name}`; the model could not \
                             tell them apart (node `{id}`)"
                        )));
                    }
                    tool_nodes.push(id.clone());
                }
                Node::Tts { id, provider, voice } => {
                    reject_duplicate(&tts, node)?;
                    let provider = providers.tts().require(provider)?;
                    tts = Some((provider, id.clone(), voice.clone()));
                }
                // Explicitly refused rather than skipped. A router that is
                // accepted and then ignored turns "send hard questions to the
                // cloud model" into "send everything to whichever model
                // resolved", which is worse than refusing to run the graph.
                Node::Router { id, .. } => {
                    return Err(Error::Config(format!(
                        "`router` nodes are not executable yet, and running this graph \
                         would ignore the routing it describes (node `{id}`)"
                    )))
                }
                other => {
                    return Err(Error::Config(format!(
                        "`{}` nodes are not executable yet (node `{}`)",
                        other.kind_name(),
                        other.id()
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

/// The model a language model node asks for.
///
/// A node names a provider *definition*, and a definition is shared: two
/// pipelines pointing at one provider must be able to reason with different
/// models, so the model belongs to the node. A node that names none keeps the
/// old behavior as an explicit choice — whichever model the provider serves
/// first, or, for a provider that serves none and therefore passes any name
/// through, the definition id itself, because definition ids cannot spell a tag
/// like `qwen3:8b` in the first place.
fn resolve_model(
    provider: &dyn LanguageModel,
    node: &Node,
    requested: Option<&str>,
) -> Result<String> {
    let Some(requested) = requested else {
        return Ok(provider
            .models()
            .first()
            .cloned()
            .unwrap_or_else(|| node.provider().to_owned()));
    };

    // An empty list means the provider passes any name through, so there is
    // nothing to check it against. A non-empty one is what the provider says
    // it can serve, and asking for anything else fails at the first token —
    // long after the operator stopped looking at the graph they just saved.
    let served = provider.models();
    if served.is_empty() || served.iter().any(|model| model == requested) {
        return Ok(requested.to_owned());
    }

    Err(Error::Config(format!(
        "node `{}` asks provider `{}` for model `{requested}`, which it does not serve; \
         it serves {}",
        node.id(),
        node.provider(),
        served.join(", ")
    )))
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
            node.kind_name(),
            node.id()
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
