//! Turning a validated graph into the concrete providers that will run it.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::graph::{Node, NodeKind, PipelineGraph};
use conduit_core::{Error, Result};
use conduit_provider::llm::{LanguageModel, ToolSpec};
use conduit_provider::stt::SpeechToText;
use conduit_provider::tool::Tool;
use conduit_provider::tts::TextToSpeech;
use serde::Deserialize;

use crate::Providers;

/// Configuration read from a [`NodeKind::Llm`] node.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LlmConfig {
    /// Model identifier passed through to the provider.
    model: Option<String>,
    /// System prompt prepended to every turn.
    system: Option<String>,
    /// Cap on model calls in one turn. See [`Plan::max_tool_rounds`].
    max_tool_rounds: Option<usize>,
}

/// How many times a model may be called in one turn before the runtime stops.
///
/// A model that keeps requesting tools would otherwise loop forever while the
/// person who asked the question waits.
const DEFAULT_MAX_TOOL_ROUNDS: usize = 4;

/// Configuration read from a [`NodeKind::Tts`] node.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TtsConfig {
    /// Voice identifier, or the provider's default when absent.
    voice: Option<String>,
}

/// The resolved providers and settings for one pipeline.
///
/// Resolution happens once, at prepare time, so a turn never pays for a
/// registry lookup and a misconfigured graph fails before any audio arrives.
pub struct Plan {
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
    /// System prompt, when the node configured one.
    pub system: Option<String>,
    /// Synthesizer.
    pub tts: Arc<dyn TextToSpeech>,
    /// Node id of the synthesizer.
    pub tts_node: String,
    /// Voice to request, when the node configured one.
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
    /// cannot execute yet or a node is missing required configuration.
    pub fn resolve(graph: &PipelineGraph, providers: &Providers) -> Result<Self> {
        let mut stt = None;
        let mut llm = None;
        let mut tts = None;
        let mut tools = BTreeMap::new();

        for node in graph.topological_order()? {
            match node.kind {
                // Endpoints describe where audio enters and leaves; the
                // caller supplies both, so there is nothing to resolve.
                NodeKind::Source | NodeKind::Router | NodeKind::Sink => {}
                NodeKind::Stt => {
                    reject_duplicate(&stt, node)?;
                    stt = Some((providers.stt().require(&node.provider)?, node.id.clone()));
                }
                NodeKind::Llm => {
                    reject_duplicate(&llm, node)?;
                    let config: LlmConfig = parse_config(node)?;
                    let model = config.model.ok_or_else(|| {
                        Error::Config(format!(
                            "node `{}` needs a `model` in its configuration",
                            node.id
                        ))
                    })?;
                    llm = Some((
                        providers.llm().require(&node.provider)?,
                        node.id.clone(),
                        model,
                        config.system,
                        config.max_tool_rounds.unwrap_or(DEFAULT_MAX_TOOL_ROUNDS),
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
                }
                NodeKind::Tts => {
                    reject_duplicate(&tts, node)?;
                    let config: TtsConfig = parse_config(node)?;
                    tts = Some((
                        providers.tts().require(&node.provider)?,
                        node.id.clone(),
                        config.voice,
                    ));
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

        if !tools.is_empty() && !llm.supports_tools() {
            return Err(Error::Config(format!(
                "node `{llm_node}` uses provider `{}`, which cannot call tools, but the \
                 pipeline defines {} of them",
                llm.name(),
                tools.len()
            )));
        }

        Ok(Self {
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

/// Reads a node's configuration into `T`.
fn parse_config<T: Default + serde::de::DeserializeOwned>(node: &Node) -> Result<T> {
    if node.config.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(node.config.clone()).map_err(|error| {
        Error::Config(format!("node `{}` has invalid configuration: {error}", node.id))
    })
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
