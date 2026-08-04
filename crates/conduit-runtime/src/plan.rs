//! Turning a validated graph into the concrete providers that will run it.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::graph::{
    ConfirmPolicy, MemoryBinding, MemoryMode, Modality, Node, PipelineGraph, ToolBinding,
};
use conduit_core::memory::Scope;
use conduit_core::{Error, Result};
use conduit_provider::llm::{LanguageModel, ToolSpec};
use conduit_provider::memory::Memory;
use conduit_provider::speaker::SpeakerIdentifier;
use conduit_provider::stt::SpeechToText;
use conduit_provider::tool::Tool;
use conduit_provider::transform::UtteranceTransform;
use conduit_provider::tts::TextToSpeech;
use conduit_provider::wake::{WakePhrase, WakeWordDetector};

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

/// The wake word detector a pipeline resolved, and the node that chose it.
///
/// `None` on a plan means the pipeline listens continuously: every turn it is
/// given is one somebody already decided to start, whether by pressing a button
/// or by typing.
pub struct Detector {
    /// The provider that scores activations.
    pub provider: Arc<dyn WakeWordDetector>,
    /// Node id of the wake stage.
    pub node: String,
    /// Phrases to listen for, with the thresholds the definition set.
    pub phrases: Vec<WakePhrase>,
}

/// The speaker identifier a pipeline resolved, and the node that chose it.
pub struct Identifier {
    /// The provider that matches a voice against enrolled prints.
    pub provider: Arc<dyn SpeakerIdentifier>,
    /// Node id of the identification stage.
    pub node: String,
}

/// One utterance transform a pipeline resolved, and the node that chose it.
pub struct Rewriter {
    /// The provider that rewrites.
    pub provider: Arc<dyn UtteranceTransform>,
    /// Node id of the transform stage.
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
    pub tools: BTreeMap<String, BoundTool>,
    /// Memory this core retrieves from and stores to.
    ///
    /// Empty today: resolution refuses every mode rather than accepting a
    /// binding it would not run, so nothing reaches this field until track F
    /// executes memory. It is here because the alternative — dropping the
    /// binding at resolution — is what the refusal exists to avoid.
    pub memory: Vec<ResolvedMemory>,
    /// Cap on model calls in one turn.
    pub max_rounds: usize,
}

impl CorePlan {
    /// The tool schemas to advertise to the model.
    #[must_use]
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|bound| bound.tool.spec()).collect()
    }
}

/// The resolved providers and settings for one pipeline.
///
/// Resolution happens once, at prepare time, so a turn never pays for a
/// registry lookup and a misconfigured graph fails before any audio arrives.
pub struct Plan {
    /// Pipeline this plan executes.
    pub pipeline: String,
    /// Wake word detector, and the node that selected it.
    ///
    /// `None` for a pipeline that listens continuously: nothing gates the
    /// audio, because every turn it is given is one somebody already started.
    pub wake: Option<Detector>,
    /// Speaker identifier, and the node that selected it.
    ///
    /// `None` for a pipeline that does not care who is speaking. A turn with no
    /// identifier reaches a tool's permission check with no speaker, which is
    /// what every pipeline did before identification existed.
    pub speaker: Option<Identifier>,
    /// Recognizer, and the node that selected it.
    ///
    /// `None` for a pipeline fed by text. Absence is what says the input is
    /// already words: a graph that carried audio to the model without
    /// transcribing it would fail modality validation long before here, so
    /// there is no third case where audio arrives with nothing to hear it.
    pub stt: Option<Recognizer>,
    /// The model that answers, and everything it may reach for.
    pub core: CorePlan,
    /// Whether the graph writes the reply down as well as, or instead of,
    /// speaking it.
    ///
    /// A pipeline may do both: a hybrid graph feeds one core from a microphone
    /// and a chat box and delivers to a speaker and a transcript, so the same
    /// segment is spoken and written.
    pub writes_text: bool,
    /// Synthesizer, and the node that selected it.
    ///
    /// `None` for a pipeline that writes its reply down instead of speaking
    /// it. Speech is one rendering of an utterance and text is the other, so
    /// which one a pipeline uses is a property of its graph rather than of the
    /// model that produced the words.
    pub tts: Option<Synthesizer>,
    /// Rewrites to apply before each rendering.
    pub transforms: Transforms,
}

/// The transform chains a pipeline resolved, one per rendering.
///
/// Kept apart because that is the point of putting transforms in the graph:
/// the markdown a model wrote belongs in a transcript and not in a voice, so
/// a pipeline that does both needs to say which rewrites apply to which.
#[derive(Default)]
pub struct Transforms {
    /// Applied to each segment before it is synthesized.
    pub speech: Vec<Rewriter>,
    /// Applied to each segment before it is written to a text sink.
    pub text: Vec<Rewriter>,
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
        let mut wake = None;
        let mut speaker = None;
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
                Node::WakeWord { id, provider } => {
                    reject_duplicate(&wake, node)?;
                    let detector = providers.wake().require(provider)?;
                    // The phrases come from the provider definition rather than
                    // from the node: which words wake a house is a property of
                    // the detector that was configured, and two pipelines
                    // pointing at one detector cannot listen for different ones.
                    let phrases = detector.descriptor().metadata.phrases.clone();
                    wake = Some(Detector { provider: detector, node: id.clone(), phrases });
                }
                Node::SpeakerId { id, provider } => {
                    reject_duplicate(&speaker, node)?;
                    speaker = Some(Identifier {
                        provider: providers.speaker().require(provider)?,
                        node: id.clone(),
                    });
                }
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
                        memory.push(resolve_memory(binding, providers)?);
                    }
                    let system = combined_system(llm.system_prompt(), core.system.as_deref());
                    reasoning = Some(Reasoning {
                        node: id.clone(),
                        llm,
                        model,
                        system,
                        max_rounds: core.max_rounds,
                    });
                }
                // Resolved from the renderer they feed rather than from the
                // walk, because a transform means nothing on its own: which
                // rendering it changes is what the operator wrote it down to
                // say, and that is a property of its edges.
                Node::Transform { .. } => {}
                Node::Tts { id, provider, voice } => {
                    reject_duplicate(&tts, node)?;
                    tts = Some(Synthesizer {
                        provider: providers.tts().require(provider)?,
                        node: id.clone(),
                        voice: voice.clone(),
                    });
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

        if !tools.is_empty() && !reasoning.llm.descriptor().metadata.tools {
            return Err(Error::Config(format!(
                "node `{}` uses provider `{}`, which cannot call tools, but the \
                 pipeline defines {} of them",
                reasoning.node,
                reasoning.llm.name(),
                tools.len()
            )));
        }

        let transforms = resolve_transforms(graph, tts.as_ref(), providers)?;

        let Reasoning { node, llm, model, system, max_rounds } = reasoning;
        Ok(Self {
            pipeline: graph.name.clone(),
            wake,
            speaker,
            stt,
            core: CorePlan { node, llm, model, system, tools, memory, max_rounds },
            tts,
            transforms,
            // Validation has already established that every sink is fed by the
            // core, so a text sink existing is enough to know the reply is
            // written down.
            writes_text: graph
                .nodes
                .iter()
                .any(|node| matches!(node, Node::Sink { modality: Modality::Text, .. })),
        })
    }
}

/// Resolves the transform chain feeding each rendering.
///
/// Every transform in the graph must end up in one of them. A transform that
/// feeds nothing is a rewrite an operator wrote down and will never hear, and
/// the difference between that and a working pipeline is one edge — so it is
/// reported here rather than discovered when the emoji come out anyway.
fn resolve_transforms(
    graph: &PipelineGraph,
    tts: Option<&Synthesizer>,
    providers: &Providers,
) -> Result<Transforms> {
    let speech = match tts {
        Some(synthesizer) => chain_into(graph, &synthesizer.node, providers)?,
        None => Vec::new(),
    };

    let mut text: Vec<Rewriter> = Vec::new();
    let mut first_sink: Option<&str> = None;
    for node in &graph.nodes {
        let Node::Sink { id, modality: Modality::Text, .. } = node else {
            continue;
        };
        let chain = chain_into(graph, id, providers)?;
        match first_sink {
            None => {
                first_sink = Some(id);
                text = chain;
            }
            // One written rendering is delivered per turn, so two text sinks
            // asking for different rewrites is a pipeline that cannot say what
            // the transcript should read.
            Some(first) if !same_chain(&text, &chain) => {
                return Err(Error::Config(format!(
                    "text sinks `{first}` and `{id}` are fed by different transforms, but \
                     one turn writes one transcript; feed them from the same transform or \
                     keep one of them"
                )));
            }
            Some(_) => {}
        }
    }

    let mut unused = graph
        .nodes
        .iter()
        .filter(|node| matches!(node, Node::Transform { .. }))
        .map(Node::id)
        .filter(|id| !applied(&speech, id) && !applied(&text, id));
    if let Some(orphan) = unused.next() {
        return Err(Error::Config(format!(
            "node `{orphan}` is a `transform` that nothing renders through; wire it into \
             the `tts` node or a text sink, or remove it"
        )));
    }

    Ok(Transforms { speech, text })
}

/// The transforms feeding `target`, in the order they run.
///
/// Walked backwards from the renderer because that is the question being
/// asked: not "what transforms exist" but "what happens to what this one
/// speaks".
fn chain_into(
    graph: &PipelineGraph,
    target: &str,
    providers: &Providers,
) -> Result<Vec<Rewriter>> {
    let mut chain: Vec<(&str, &str)> = Vec::new();
    let mut current = target;

    loop {
        let mut upstream =
            graph.edges.iter().filter(|edge| edge.to == current).filter_map(|edge| match graph
                .node(&edge.from)
            {
                Some(Node::Transform { id, provider }) => {
                    Some((id.as_str(), provider.as_str()))
                }
                _ => None,
            });

        let Some(found) = upstream.next() else {
            break;
        };
        if upstream.next().is_some() {
            return Err(Error::Config(format!(
                "node `{current}` is fed by more than one `transform`, so which rewrite \
                 runs last is undecided; chain them instead"
            )));
        }
        // A graph is acyclic by the time it is resolved, so this only guards
        // against resolving one that was never validated.
        if chain.iter().any(|(id, _)| *id == found.0) {
            break;
        }
        chain.push(found);
        current = found.0;
    }

    chain.reverse();
    chain
        .into_iter()
        .map(|(id, provider)| {
            Ok(Rewriter {
                provider: providers.transform().require(provider)?,
                node: id.to_owned(),
            })
        })
        .collect()
}

/// Whether two chains name the same transforms in the same order.
fn same_chain(left: &[Rewriter], right: &[Rewriter]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| left.node == right.node)
}

/// A chain as `node (provider)` pairs, for a plan an operator is reading.
fn node_names(chain: &[Rewriter]) -> Vec<String> {
    chain
        .iter()
        .map(|rewriter| format!("{} ({})", rewriter.node, rewriter.provider.name()))
        .collect()
}

/// Whether `id` appears in `chain`.
fn applied(chain: &[Rewriter], id: &str) -> bool {
    chain.iter().any(|rewriter| rewriter.node == id)
}

/// A memory store a core uses, and how it uses it.
pub struct ResolvedMemory {
    /// The store itself.
    pub provider: Arc<dyn Memory>,
    /// Whether this pipeline reads from it, writes to it, or both.
    pub mode: MemoryMode,
    /// Which scope to search, or every scope when `None`.
    pub scope: Option<Scope>,
    /// How many records one retrieval may return.
    pub limit: usize,
}

/// A tool a core offers, and whether to ask before running it.
///
/// The policy travels with the tool because the question is asked at dispatch,
/// where the graph is long out of reach.
pub struct BoundTool {
    /// The tool itself.
    pub tool: Arc<dyn Tool>,
    /// Whether this pipeline wants to be asked before it runs.
    pub confirm: ConfirmPolicy,
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
    tools: &mut BTreeMap<String, BoundTool>,
    providers: &Providers,
    binding: &ToolBinding,
    node: &str,
) -> Result<()> {
    let tool = providers.tools().require(&binding.provider)?;
    let name = tool.spec().name;
    if tools.insert(name.clone(), BoundTool { tool, confirm: binding.confirm }).is_some() {
        return Err(Error::Config(format!(
            "two tools are both called `{name}`; the model could not tell them apart \
             (node `{node}`)"
        )));
    }
    Ok(())
}

/// Joins the provider definition's system prompt with this pipeline's.
///
/// The definition's comes first because it is the wider statement — what this
/// endpoint should be, inherited by every pipeline pointing at it — and the
/// pipeline's narrows it. Replacing rather than appending would let one
/// pipeline quietly drop a deployment-wide instruction.
fn combined_system(definition: Option<&str>, pipeline: Option<&str>) -> Option<String> {
    match (definition, pipeline) {
        (Some(definition), Some(pipeline)) => Some(format!("{definition}\n\n{pipeline}")),
        (Some(only), None) | (None, Some(only)) => Some(only.to_owned()),
        (None, None) => None,
    }
}

/// Resolves one memory binding against the registered stores.
fn resolve_memory(binding: &MemoryBinding, providers: &Providers) -> Result<ResolvedMemory> {
    Ok(ResolvedMemory {
        provider: providers.memory().require(&binding.provider)?,
        mode: binding.mode,
        scope: binding.scope,
        limit: binding.limit,
    })
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
        // The definition's first served model, when it serves any. A
        // definition that serves none and a node that names none leave nothing
        // to ask for — and the id is not an answer: asking OpenAI for a model
        // called `openai` fails at the first token, which is a long way from
        // the form where the model was never filled in.
        return provider.descriptor().metadata.models.first().cloned().ok_or_else(|| {
            Error::Config(format!(
                "node `{}` names no model and provider `{}` advertises none, so there \
                 is nothing to ask for; set a model on the node or on the provider \
                 definition",
                node.id(),
                node.provider()
            ))
        });
    };

    // An empty list means the provider passes any name through, so there is
    // nothing to check it against. A non-empty one is what the provider says
    // it can serve, and asking for anything else fails at the first token —
    // long after the operator stopped looking at the graph they just saved.
    let served = &provider.descriptor().metadata.models;
    if provider.descriptor().metadata.serves_model(requested) {
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

/// Written by hand because a plan holds trait objects, which are not `Debug`.
/// Shows the resolved wiring, which is what anyone printing a plan wants.
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field(
                "wake",
                &self
                    .wake
                    .as_ref()
                    .map(|wake| format!("{} ({})", wake.node, wake.provider.name())),
            )
            .field(
                "speaker",
                &self
                    .speaker
                    .as_ref()
                    .map(|speaker| format!("{} ({})", speaker.node, speaker.provider.name())),
            )
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
            .field("spoken through", &node_names(&self.transforms.speech))
            .field("written through", &node_names(&self.transforms.text))
            .field("tools", &self.core.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}
