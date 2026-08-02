//! Turning a validated graph into the concrete providers that will run it.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::graph::{
    ConfirmPolicy, MemoryBinding, MemoryMode, Node, PipelineGraph, ToolBinding,
};
use conduit_core::{Error, Result};
use conduit_provider::llm::{LanguageModel, ToolSpec};
use conduit_provider::stt::SpeechToText;
use conduit_provider::tool::Tool;
use conduit_provider::tts::TextToSpeech;

use crate::Providers;

/// The recognizer a pipeline resolved, and the node that chose it.
///
/// Paired because every failure report needs both: the provider does the work
/// and the node id is what an operator can find in their graph.
pub struct Recognizer {
    /// The provider that transcribes.
    pub provider: Arc<dyn SpeechToText>,
    /// Node id of the recognizer.
    pub node: String,
}

/// The synthesizer a pipeline resolved, and the node that chose it.
///
/// `None` on a plan means the reply is delivered as text: a pipeline with no
/// synthesizer renders what the model said by writing it down.
pub struct Synthesizer {
    /// The provider that speaks.
    pub provider: Arc<dyn TextToSpeech>,
    /// Node id of the synthesizer.
    pub node: String,
    /// Voice this pipeline's synthesis node asks for, when present.
    pub voice: Option<String>,
}

/// The reasoning core a pipeline resolved: one model, and what it may reach
/// for while answering.
///
/// A turn reads everything about reasoning from here, which is what lets a
/// graph spell the same pipeline as a `core` node or as an `llm` beside its
/// tool and memory nodes. The two shapes resolve to one plan, so nothing
/// downstream of resolution knows which was written.
pub struct CorePlan {
    /// Node id of the core, for reports an operator can find in their graph.
    pub node: String,
    /// The provider that reasons.
    pub llm: Arc<dyn LanguageModel>,
    /// Model identifier to request.
    pub model: String,
    /// System prompt this pipeline asks for, when present.
    pub system: Option<String>,
    /// Tools offered to the model, keyed by the name it calls them by.
    ///
    /// Unlike the transport stages a core may reach for any number of these,
    /// so they are collected rather than treated as one slot.
    pub tools: BTreeMap<String, Arc<dyn Tool>>,
    /// Memory this core retrieves from and stores to.
    ///
    /// Empty today: resolution refuses every mode rather than accepting a
    /// binding it would not run, so nothing reaches this field until track F
    /// executes memory. It is here because the alternative — dropping the
    /// binding at resolution — is what the refusal exists to avoid.
    pub memory: Vec<MemoryBinding>,
    /// Cap on model calls in one turn.
    pub max_rounds: usize,
}

impl CorePlan {
    /// The tool schemas to advertise to the model.
    #[must_use]
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }
}

/// The resolved providers and settings for one pipeline.
///
/// Resolution happens once, at prepare time, so a turn never pays for a
/// registry lookup and a misconfigured graph fails before any audio arrives.
pub struct Plan {
    /// Pipeline this plan executes.
    pub pipeline: String,
    /// Recognizer, and the node that selected it.
    ///
    /// `None` for a pipeline fed by text. Absence is what says the input is
    /// already words: a graph that carried audio to the model without
    /// transcribing it would fail modality validation long before here, so
    /// there is no third case where audio arrives with nothing to hear it.
    pub stt: Option<Recognizer>,
    /// The model that answers, and everything it may reach for.
    pub core: CorePlan,
    /// Synthesizer, and the node that selected it.
    ///
    /// `None` for a pipeline that writes its reply down instead of speaking
    /// it. Speech is one rendering of an utterance and text is the other, so
    /// which one a pipeline uses is a property of its graph rather than of the
    /// model that produced the words.
    pub tts: Option<Synthesizer>,
}

impl Plan {
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
        let mut reasoning = None;
        let mut tts = None;
        let mut tools = BTreeMap::new();
        let mut memory = Vec::new();

        for node in graph.topological_order()? {
            match node {
                // Endpoints describe where audio enters and leaves; the
                // caller supplies both, so there is nothing to resolve.
                Node::Source { .. } | Node::Sink { .. } => {}
                Node::Stt { id, provider } => {
                    reject_duplicate(&stt, node)?;
                    stt = Some(Recognizer {
                        provider: providers.stt().require(provider)?,
                        node: id.clone(),
                    });
                }
                Node::Core { id, core } => {
                    reject_duplicate(&reasoning, node)?;
                    let llm = providers.llm().require(&core.model.provider)?;
                    let model = resolve_model(llm.as_ref(), node, core.model.model.as_deref())?;
                    for binding in &core.tools {
                        offer_tool(&mut tools, providers, binding, id)?;
                    }
                    for binding in &core.memory {
                        memory.push(resolve_memory(binding, id)?);
                    }
                    reasoning = Some(Reasoning {
                        node: id.clone(),
                        llm,
                        model,
                        system: core.system.clone(),
                        max_rounds: core.max_rounds,
                    });
                }
                Node::Tts { id, provider, voice } => {
                    reject_duplicate(&tts, node)?;
                    tts = Some(Synthesizer {
                        provider: providers.tts().require(provider)?,
                        node: id.clone(),
                        voice: voice.clone(),
                    });
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

        let reasoning = reasoning.ok_or_else(|| {
            Error::Config("pipeline has no `core` node, so nothing would answer".to_owned())
        })?;
        // Nothing renders the reply unless the graph either speaks it or
        // writes it somewhere. A sink is what "writes it somewhere" looks
        // like, so a graph with neither would reason and then discard.
        if tts.is_none() && !graph.nodes.iter().any(|node| matches!(node, Node::Sink { .. })) {
            return Err(Error::Config(
                "pipeline has no `tts` node and no `sink`, so nothing would deliver the \
                 reply"
                    .to_owned(),
            ));
        }

        // This runtime executes recognition, then reasoning, then synthesis,
        // in that order. A graph is only a description of *this* pipeline if
        // its edges say the same thing.
        //
        // Modality typing catches the backwards graph, `tts -> llm -> stt`,
        // structurally now. It cannot catch a graph that branches past the
        // model — `stt -> llm` beside `stt -> tts` — because every edge there
        // carries something its far end reads, and the defect is a missing
        // path rather than a bad edge. Reachability still has to be asked
        // about; `crates/conduit-runtime/tests/plan.rs` pins that shape.
        //
        // A core's tools and memory are not asked about at all, and not by
        // omission: a binding is configuration rather than a stage, so there
        // is no edge for it to be reachable over.
        if let Some(recognizer) = &stt {
            require_downstream(graph, &recognizer.node, &reasoning.node)?;
        }
        if let Some(synthesizer) = &tts {
            require_downstream(graph, &reasoning.node, &synthesizer.node)?;
        }
        if !tools.is_empty() && !reasoning.llm.supports_tools() {
            return Err(Error::Config(format!(
                "node `{}` uses provider `{}`, which cannot call tools, but the \
                 pipeline defines {} of them",
                reasoning.node,
                reasoning.llm.name(),
                tools.len()
            )));
        }

        let Reasoning { node, llm, model, system, max_rounds } = reasoning;
        Ok(Self {
            pipeline: graph.name.clone(),
            stt,
            core: CorePlan { node, llm, model, system, tools, memory, max_rounds },
            tts,
        })
    }
}

/// One model and its settings, before its tools and memory join it.
///
/// Tools arrive from a core's bindings *and* from tool nodes, so they are
/// collected across the whole walk and attached once at the end. Keeping them
/// apart until then is what lets one loop resolve either spelling.
struct Reasoning {
    node: String,
    llm: Arc<dyn LanguageModel>,
    model: String,
    system: Option<String>,
    max_rounds: usize,
}

/// Adds a tool to the set the model will be offered.
///
/// The model picks a tool by name, so two tools answering to one name is a
/// pipeline where it cannot say which it meant.
fn offer_tool(
    tools: &mut BTreeMap<String, Arc<dyn Tool>>,
    providers: &Providers,
    binding: &ToolBinding,
    node: &str,
) -> Result<()> {
    // Modelled now, enforced in track G. Dispatching a tool the operator asked
    // to be consulted about is the one outcome the setting was chosen to
    // prevent, so accepting it and running it anyway is worse than refusing.
    if binding.confirm == ConfirmPolicy::Always {
        return Err(Error::Config(format!(
            "tool `{}` on node `{node}` is bound with `confirm: always`, and this \
             runtime cannot ask before dispatching yet; it would call the tool unasked",
            binding.provider
        )));
    }

    let tool = providers.tools().require(&binding.provider)?;
    let name = tool.spec().name;
    if tools.insert(name.clone(), tool).is_some() {
        return Err(Error::Config(format!(
            "two tools are both called `{name}`; the model could not tell them apart \
             (node `{node}`)"
        )));
    }
    Ok(())
}

/// Resolves a memory binding, or explains why it cannot run yet.
///
/// Every mode is refused today, because retrieval and storage are track F. The
/// refusal is per mode rather than blanket so that track F can turn them on one
/// at a time — and it is a refusal rather than a silent drop for the reason the
/// router refusal is: a pipeline told to remember what was said, which answers
/// as though nothing was, has nothing to show that it ignored the instruction.
fn resolve_memory(binding: &MemoryBinding, node: &str) -> Result<MemoryBinding> {
    let asked = match binding.mode {
        MemoryMode::Read => "retrieve what was said before",
        MemoryMode::Write => "store what is said now",
        MemoryMode::ReadWrite => "retrieve what was said before and store what is said now",
    };
    Err(Error::Config(format!(
        "memory `{}` on node `{node}` asks to {asked}, and this runtime cannot execute \
         memory yet; the pipeline would answer as though it had none",
        binding.provider
    )))
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

/// Written by hand because a plan holds trait objects, which are not `Debug`.
/// Shows the resolved wiring, which is what anyone printing a plan wants.
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field(
                "stt",
                &self.stt.as_ref().map(|stt| format!("{} ({})", stt.node, stt.provider.name())),
            )
            .field("core", &format_args!("{} ({})", self.core.node, self.core.llm.name()))
            .field("model", &self.core.model)
            .field("system", &self.core.system)
            .field(
                "tts",
                &self.tts.as_ref().map(|tts| format!("{} ({})", tts.node, tts.provider.name())),
            )
            .field("voice", &self.tts.as_ref().and_then(|tts| tts.voice.as_deref()))
            .field("tools", &self.core.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}
