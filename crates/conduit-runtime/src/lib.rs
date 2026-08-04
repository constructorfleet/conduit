//! The Conduit runtime: executes a stored pipeline graph.
//!
//! [`Runner::prepare`] resolves a [`PipelineGraph`] against the registered
//! providers once, and [`Runner::run`] then executes a turn per utterance —
//! audio in, speech out, events throughout.
//!
//! The runtime executes one wake stage, one identification stage, one
//! recognizer, one model, one synthesizer, and any number of tool branches.
//! Each of those stages is optional except the model. A graph naming a stage
//! twice, or naming a provider the deployment does not have, is rejected at
//! prepare time rather than silently mishandled.
//!
//! # Example
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use conduit_core::bus::EventBus;
//! # use conduit_core::graph::PipelineGraph;
//! # use conduit_runtime::{Providers, Runner};
//! # fn example(graph: &PipelineGraph, providers: &Providers) -> conduit_core::Result<()> {
//! let runner = Runner::prepare(graph, providers, EventBus::default())?;
//! # let audio = todo!();
//! let conversation = runner.run(audio);
//! // Events for this turn are tagged with `conversation.id`.
//! # Ok(())
//! # }
//! ```

pub mod confirm;
pub mod deadline;
mod emit;
pub mod identity;
pub mod plan;
pub mod sentences;
pub mod stop;
pub mod tools;
mod turn;
pub mod wake;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::id::{ConversationId, DeviceId, SpeakerId};
use conduit_core::Result;
use conduit_provider::llm::LanguageModel;
use conduit_provider::memory::Memory;
use conduit_provider::speaker::SpeakerIdentifier;
use conduit_provider::stt::{AudioChunk, SpeechToText};
use conduit_provider::tool::Tool;
use conduit_provider::transform::UtteranceTransform;
use conduit_provider::tts::{SpeechChunk, TextToSpeech};
use conduit_provider::wake::WakeWordDetector;
use conduit_provider::{Capability, ChunkStream, Provider, Registry, RegistryHandle};
use futures_util::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument;

pub use confirm::{ConfirmationListener, Confirmations};
pub use deadline::DEFAULT_IDLE_TIMEOUT;
pub use plan::Plan;
pub use stop::Stop;
pub use turn::Reply;
pub use turn::TurnInput;

/// How many synthesized chunks may be queued before synthesis waits.
///
/// The bound is the backpressure: if a device stops draining audio, the
/// runtime stops producing it rather than buffering a whole response.
const OUTPUT_BUFFER: usize = 16;

/// The providers available to a pipeline, one registry per capability.
///
/// Capability-indexed rather than one hand-written field per capability: the
/// bundle holds a [`Registry`] behind a type-erased [`RegistryHandle`] for
/// every [`Capability`], so registering and enumerating wake, speaker, or
/// memory providers goes through the exact same path recognition, reasoning,
/// synthesis, and tools always have. Supporting a new capability is adding a
/// [`Capability`] variant and one typed accessor pair — never an edit to this
/// struct, [`Providers::new`], or [`Providers`]'s [`Debug`](std::fmt::Debug)
/// output.
pub struct Providers {
    registries: BTreeMap<Capability, Box<dyn RegistryHandle>>,
}

impl Providers {
    /// An empty set of providers.
    ///
    /// Every capability gets an empty registry up front — from
    /// [`Capability::ALL`], which is the one place that lists them — so a
    /// typed accessor never has to handle "this capability was never touched"
    /// as a special case; it reads as an ordinary empty [`Registry`].
    #[must_use]
    pub fn new() -> Self {
        let registries = Capability::ALL
            .into_iter()
            .map(|capability| (capability, empty_registry(capability)))
            .collect();
        Self { registries }
    }

    /// The registry for `capability`, downcast to its concrete type.
    ///
    /// # Panics
    ///
    /// Never in practice: [`Providers::new`] seeds every [`Capability`], and
    /// [`empty_registry`] and every typed accessor agree on which
    /// [`Registry`] type backs each one.
    fn registry<T: Provider + ?Sized>(&self, capability: Capability) -> &Registry<T> {
        self.registries
            .get(&capability)
            .expect("Providers::new seeds every capability")
            .as_any()
            .downcast_ref::<Registry<T>>()
            .expect("empty_registry agrees with the typed accessor on the registry's type")
    }

    /// The registry for `capability`, downcast to its concrete type, mutably.
    ///
    /// # Panics
    ///
    /// See [`Providers::registry`].
    fn registry_mut<T: Provider + ?Sized>(
        &mut self,
        capability: Capability,
    ) -> &mut Registry<T> {
        self.registries
            .get_mut(&capability)
            .expect("Providers::new seeds every capability")
            .as_any_mut()
            .downcast_mut::<Registry<T>>()
            .expect("empty_registry agrees with the typed accessor on the registry's type")
    }

    /// Registers a provider of any capability under `name`.
    ///
    /// The single uniform path every `with_*` builder routes through: what
    /// differs between registering a recognizer and registering a wake word
    /// detector is only which [`Capability`] and which registry type the
    /// caller names, not the mechanism.
    fn with<T: Provider + ?Sized>(
        mut self,
        capability: Capability,
        name: impl Into<String>,
        provider: Arc<T>,
    ) -> Self {
        self.registry_mut::<T>(capability).insert(name, provider);
        self
    }

    /// Registers a recognizer under its own [`Provider::name`].
    ///
    /// [`Provider::name`]: conduit_provider::Provider::name
    #[must_use]
    pub fn with_stt<P: SpeechToText>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn SpeechToText>(Capability::Stt, name, Arc::new(provider))
    }

    /// Registers a language model under its own name.
    #[must_use]
    pub fn with_llm<P: LanguageModel>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn LanguageModel>(Capability::Llm, name, Arc::new(provider))
    }

    /// Registers a synthesizer under its own name.
    #[must_use]
    pub fn with_tts<P: TextToSpeech>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn TextToSpeech>(Capability::Tts, name, Arc::new(provider))
    }

    /// Registers an utterance transform under its own name.
    #[must_use]
    pub fn with_transform<P: UtteranceTransform>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn UtteranceTransform>(Capability::Transform, name, Arc::new(provider))
    }

    /// Registers a tool under its own name.
    ///
    /// The name a graph node refers to is the provider name; the name the
    /// model calls it by comes from the tool's own schema.
    #[must_use]
    pub fn with_tool<P: Tool>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn Tool>(Capability::Tool, name, Arc::new(provider))
    }

    /// Registers a memory store under its own name.
    #[must_use]
    pub fn with_memory<P: Memory>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn Memory>(Capability::Memory, name, Arc::new(provider))
    }

    /// Registers a wake word detector under its own name.
    #[must_use]
    pub fn with_wake<P: WakeWordDetector>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn WakeWordDetector>(Capability::Wake, name, Arc::new(provider))
    }

    /// Registers a speaker identifier under its own name.
    #[must_use]
    pub fn with_speaker<P: SpeakerIdentifier>(self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.with::<dyn SpeakerIdentifier>(Capability::SpeakerId, name, Arc::new(provider))
    }

    /// The registered recognizers.
    #[must_use]
    pub fn stt(&self) -> &Registry<dyn SpeechToText> {
        self.registry(Capability::Stt)
    }

    /// The registered language models.
    #[must_use]
    pub fn llm(&self) -> &Registry<dyn LanguageModel> {
        self.registry(Capability::Llm)
    }

    /// The registered synthesizers.
    #[must_use]
    pub fn tts(&self) -> &Registry<dyn TextToSpeech> {
        self.registry(Capability::Tts)
    }

    /// The registered utterance transforms.
    #[must_use]
    pub fn transform(&self) -> &Registry<dyn UtteranceTransform> {
        self.registry(Capability::Transform)
    }

    /// The registered tools.
    #[must_use]
    pub fn tools(&self) -> &Registry<dyn Tool> {
        self.registry(Capability::Tool)
    }

    /// The registered memory stores.
    #[must_use]
    pub fn memory(&self) -> &Registry<dyn Memory> {
        self.registry(Capability::Memory)
    }

    /// The registered wake word detectors.
    #[must_use]
    pub fn wake(&self) -> &Registry<dyn WakeWordDetector> {
        self.registry(Capability::Wake)
    }

    /// The registered speaker identifiers.
    #[must_use]
    pub fn speaker(&self) -> &Registry<dyn SpeakerIdentifier> {
        self.registry(Capability::SpeakerId)
    }

    /// Every capability with at least one registered provider, and the names
    /// registered under it — in capability order, then name order.
    ///
    /// The generic listing a status page or a diagnostic reads from: it walks
    /// every [`Capability`] uniformly rather than naming stt, llm, tts, and so
    /// on one at a time, so a capability registered after this was written
    /// still shows up in it.
    #[must_use]
    pub fn capabilities(&self) -> Vec<(Capability, Vec<String>)> {
        self.registries
            .iter()
            .filter(|(_, registry)| !registry.is_empty())
            .map(|(capability, registry)| (*capability, registry.names()))
            .collect()
    }

    /// Every registered provider, as the capability it supplies, the key it is
    /// registered under, and what it says about itself — in capability order,
    /// then key order.
    ///
    /// What an operator status page enumerates: the key is the selector a
    /// pipeline names, and the descriptor carries the identity, label and
    /// version to show beside it. Walking [`Capability`] uniformly is the
    /// point — a snapshot built by naming stt, llm and tts one at a time is a
    /// snapshot that silently omits every capability added after it was
    /// written.
    #[must_use]
    pub fn descriptors(&self) -> Vec<(Capability, String, conduit_provider::Descriptor)> {
        self.registries
            .iter()
            .flat_map(|(capability, registry)| {
                registry
                    .descriptors()
                    .into_iter()
                    .map(move |(key, descriptor)| (*capability, key, descriptor))
            })
            .collect()
    }

    /// Asks the provider registered under `key` for `capability` how it is.
    ///
    /// `None` when nothing is registered under that key, which is a different
    /// answer from an unhealthy provider: one is a selector pointing at
    /// nothing, the other is a provider that is there and cannot serve.
    pub async fn health(
        &self,
        capability: Capability,
        key: &str,
    ) -> Option<conduit_provider::Health> {
        self.registries.get(&capability)?.health(key).await
    }
}

impl Default for Providers {
    fn default() -> Self {
        Self::new()
    }
}

/// The empty, typed registry behind a fresh capability slot.
///
/// The one place that names every capability's registry type. Adding a
/// capability means adding a match arm here and the typed accessor pair on
/// [`Providers`] — [`Providers::new`] itself stays exactly as it is, because it
/// only knows [`Capability::ALL`], not which registry type answers for which
/// variant.
fn empty_registry(capability: Capability) -> Box<dyn RegistryHandle> {
    match capability {
        Capability::Stt => Box::new(Registry::<dyn SpeechToText>::new()),
        Capability::Llm => Box::new(Registry::<dyn LanguageModel>::new()),
        Capability::Tts => Box::new(Registry::<dyn TextToSpeech>::new()),
        Capability::Transform => Box::new(Registry::<dyn UtteranceTransform>::new()),
        Capability::Tool => Box::new(Registry::<dyn Tool>::new()),
        Capability::Memory => Box::new(Registry::<dyn Memory>::new()),
        Capability::Wake => Box::new(Registry::<dyn WakeWordDetector>::new()),
        Capability::SpeakerId => Box::new(Registry::<dyn SpeakerIdentifier>::new()),
    }
}

/// Written by hand because the registries hold trait objects, which are not
/// `Debug`. Lists what is registered, which is what anyone printing this wants.
///
/// Walks [`Providers::capabilities`] rather than naming each field, so a
/// capability registered after this was written is printed without an edit
/// here.
impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Providers");
        for (capability, names) in self.capabilities() {
            debug.field(capability.as_str(), &names);
        }
        debug.finish()
    }
}

/// One running turn: the reply being spoken, and the id it is filed under.
pub struct Conversation {
    /// The conversation every event from this turn carries.
    pub id: ConversationId,
    /// The reply, as it is produced.
    ///
    /// A voice pipeline yields speech and a text pipeline yields written
    /// segments. [`Conversation::speech`] narrows this to audio for a caller
    /// that only knows how to play it.
    pub output: ChunkStream<Reply>,
    /// Answers this turn's confirmation requests.
    ///
    /// A turn refuses a tool that needs confirming unless something is
    /// listening through [`Confirmations::listen`], so a deployment with no
    /// way to ask still refuses rather than waiting for an answer that is not
    /// coming.
    ///
    /// Call [`Confirmations::listen`] before reading [`Conversation::output`].
    /// The turn is already running by the time this is returned, and a
    /// listener registered after it reaches a gated tool arrives too late —
    /// the call is refused rather than waited on, because at the moment it
    /// asked there was nothing to ask.
    pub confirmations: Confirmations,
    /// Asks this turn to stop talking.
    ///
    /// Distinct from dropping [`Conversation::audio`], which also ends the turn
    /// but cannot say why: a turn that notices only a failed write cannot tell
    /// an interruption from a client that vanished. Use this when a client
    /// asked, and the cancellation is reported as
    /// [`CancelReason::UserRequested`](conduit_core::event::CancelReason::UserRequested).
    pub stop: Stop,
    /// The task running the turn, so a caller holds it rather than losing it.
    ///
    /// Kept so that a shutdown has something to wait on or abort. A turn used to
    /// be spawned and forgotten, which meant a process shutting down could not
    /// tell whether anyone was mid-sentence, and a turn wedged on a provider that
    /// never answers had nothing left that could reach it.
    ///
    /// Prefer [`Conversation::stop`] to aborting: a stop lets the turn publish
    /// why it ended, so metrics and subscribers see a cancelled conversation
    /// rather than a trace that goes silent. Aborting is for shutdown, where
    /// there may be no time left to be polite.
    turn: tokio::task::JoinHandle<()>,
}

impl Conversation {
    /// The reply as audio, dropping any written segments.
    ///
    /// For callers on an audio transport, which have nothing to do with a text
    /// segment. Failures are preserved: a turn that fails still reports it
    /// here, because a caller that hears nothing must be able to tell silence
    /// from a broken pipeline.
    pub fn speech(self) -> ChunkStream<SpeechChunk> {
        Box::pin(self.output.filter_map(|item| async move {
            match item {
                Ok(Reply::Speech(chunk)) => Some(Ok(chunk)),
                Ok(Reply::Text(_)) => None,
                Err(error) => Some(Err(error)),
            }
        }))
    }

    /// Waits for the turn to finish.
    ///
    /// Resolves once the turn has published its outcome. A turn always ends by
    /// itself eventually — it completes, it fails, it is asked to stop, or it
    /// runs out of idle time — so this cannot wait forever unless a deployment
    /// removed the deadline with [`Runner::with_idle_timeout`].
    ///
    /// # Errors
    ///
    /// Returns an error if the turn was aborted or panicked.
    pub async fn finished(self) -> std::result::Result<(), tokio::task::JoinError> {
        self.turn.await
    }

    /// Stops the turn immediately, without letting it say why.
    ///
    /// For shutdown. Anything that can afford to be polite should use
    /// [`Conversation::stop`] instead, which ends the turn *and* publishes the
    /// cancellation — an aborted turn leaves its trace simply stopping, which is
    /// indistinguishable from a crash to anyone reading the event stream.
    pub fn abort(&self) {
        self.turn.abort();
    }
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conversation").field("id", &self.id).finish_non_exhaustive()
    }
}

/// Executes one pipeline, one turn at a time.
///
/// Cheap to clone and safe to share: each [`Runner::run`] call is an
/// independent conversation with its own correlation ids.
#[derive(Clone, Debug)]
pub struct Runner {
    plan: Arc<Plan>,
    bus: EventBus,
    format: AudioFormat,
    /// How long a turn may publish nothing before it is abandoned.
    idle: Option<Duration>,
}

impl Runner {
    /// Resolves `graph` against `providers` and prepares it for execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph is invalid, names an unregistered
    /// provider, or has a topology this runtime cannot execute. See
    /// [`Plan::resolve`].
    pub fn prepare(
        graph: &PipelineGraph,
        providers: &Providers,
        bus: EventBus,
    ) -> Result<Self> {
        Ok(Self {
            plan: Arc::new(Plan::resolve(graph, providers)?),
            bus,
            format: AudioFormat::DEFAULT,
            idle: Some(DEFAULT_IDLE_TIMEOUT),
        })
    }

    /// Bounds how long a turn may publish nothing before it is abandoned.
    ///
    /// A turn that reaches the deadline is cancelled as
    /// [`CancelReason::IdleTimeout`](conduit_core::event::CancelReason::IdleTimeout)
    /// and the caller is handed an [`Error::Timeout`](conduit_core::Error::Timeout)
    /// naming the stage that went quiet. The clock restarts on every event the
    /// turn publishes, so this bounds silence rather than length: a model
    /// streaming tokens for two minutes is never abandoned, while one silent for
    /// longer than the deadline is.
    ///
    /// Defaults to [`DEFAULT_IDLE_TIMEOUT`]. `None` removes the bound, which
    /// leaves a provider that never answers holding the turn for as long as the
    /// client stays connected — reasonable only when something above the runtime
    /// already imposes a deadline of its own.
    #[must_use]
    pub const fn with_idle_timeout(mut self, idle: Option<Duration>) -> Self {
        self.idle = idle;
        self
    }

    /// Whether this pipeline's turns start from audio.
    ///
    /// A caller that produces input has to know which kind to produce, and the
    /// answer is a property of the resolved graph rather than of the request.
    #[must_use]
    pub fn expects_audio(&self) -> bool {
        self.plan.stt.is_some()
    }

    /// Sets the audio format used for capture and synthesis.
    pub fn with_format(mut self, format: AudioFormat) -> Result<Self> {
        // A pipeline fed by text has no recognizer to ask, and the format
        // still describes the audio it produces.
        if let Some(stt) = &self.plan.stt {
            if !stt.provider.descriptor().metadata.supports_encoding(format.encoding) {
                return Err(conduit_core::Error::Config(format!(
                    "node `{}` uses provider `{}`, which cannot accept {:?} audio",
                    stt.node,
                    stt.provider.name(),
                    format.encoding
                )));
            }
        }
        // A pipeline that writes its reply down has no synthesizer to ask.
        if let Some(tts) = &self.plan.tts {
            if !tts.provider.descriptor().metadata.supports_encoding(format.encoding) {
                return Err(conduit_core::Error::Config(format!(
                    "node `{}` uses provider `{}`, which cannot produce {:?} audio",
                    tts.node,
                    tts.provider.name(),
                    format.encoding
                )));
            }
        }
        self.format = format;
        Ok(self)
    }

    /// Runs one turn, returning the synthesized reply as it is produced.
    ///
    /// The returned [`Conversation`] carries the audio and the id every event
    /// from this turn is tagged with, so a caller can follow the turn on the
    /// bus without guessing which conversation is theirs.
    ///
    /// The audio stream yields before the model has finished generating.
    /// Dropping it stops the turn, cancelled as `disconnected` — a listener
    /// that left. A caller who *asked* to interrupt should use
    /// [`Conversation::stop`] instead, so the two are distinguishable.
    ///
    /// Failures arrive as error items on the stream and as `StageFailed`
    /// events on the bus.
    pub fn run(&self, audio: ChunkStream<AudioChunk>) -> Conversation {
        self.start(TurnInput::Audio(audio), None, None)
    }

    /// Runs one turn from words a client already typed.
    ///
    /// The reply is delivered the same way a spoken turn's is, so a pipeline
    /// that types its question and hears its answer is one graph rather than a
    /// special case.
    pub fn run_text(&self, text: impl Into<String>) -> Conversation {
        self.start(TurnInput::Text(text.into()), None, None)
    }

    /// Runs one turn on behalf of an identified device.
    ///
    /// Every event the turn publishes carries `device`, which is what makes
    /// `/v1/events?device=` select a single satellite. The identity must come
    /// from an authenticated device token: a value a client can choose would
    /// make the filter select whatever it claimed.
    ///
    /// A device is not a speaker. It says which satellite is connected and
    /// never who is talking, so it deliberately does not reach a tool's
    /// permission check — see [`Runner::run_as`].
    pub fn run_for_device(
        &self,
        device: DeviceId,
        audio: ChunkStream<AudioChunk>,
    ) -> Conversation {
        self.start(TurnInput::Audio(audio), None, Some(device))
    }

    /// Runs one turn attributed to an identified speaker.
    ///
    /// The speaker reaches every tool's permission check, which is how a
    /// per-speaker policy — this person may unlock the door, that one may not
    /// — becomes enforceable. Nothing calls this in production yet, because no
    /// provider identifies a voice; [`Runner::run`] is what the API uses, and
    /// tools therefore see no speaker.
    ///
    /// The identity must come from a voice. Passing a device, a token subject,
    /// or a pipeline name in this argument would make every per-speaker policy
    /// silently wrong rather than merely unenforced.
    pub fn run_as(&self, speaker: SpeakerId, audio: ChunkStream<AudioChunk>) -> Conversation {
        self.start(TurnInput::Audio(audio), Some(speaker), None)
    }

    /// Spawns a turn, which is all the `run*` methods differ in.
    fn start(
        &self,
        input: TurnInput,
        speaker: Option<SpeakerId>,
        device: Option<DeviceId>,
    ) -> Conversation {
        let (sender, receiver) = tokio::sync::mpsc::channel(OUTPUT_BUFFER);
        let stop = Stop::new();
        let confirmations = Confirmations::new();
        let mut turn = turn::Turn::new(
            Arc::clone(&self.plan),
            self.bus.clone(),
            self.format,
            sender,
            stop.clone(),
            confirmations.clone(),
            self.idle,
        );
        if let Some(speaker) = speaker {
            turn = turn.with_speaker(speaker);
        }
        if let Some(device) = device {
            turn = turn.with_device(device);
        }
        let id = turn.conversation();
        let span = tracing::info_span!("conduit.turn", conversation = %id);
        let running = tokio::spawn(turn.run(input).instrument(span));
        Conversation {
            id,
            output: Box::pin(ReceiverStream::new(receiver)),
            confirmations,
            stop,
            turn: running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::id::SpeakerId;
    use conduit_provider::memory::{Match, Memory, Query, Record};
    use conduit_provider::speaker::{Identification, SpeakerIdentifier};
    use conduit_provider::wake::{Detection, WakePhrase, WakeWordDetector};

    /// A memory store that answers nothing, standing in for a real backend in
    /// tests that only care whether it can be registered and enumerated.
    #[derive(Default)]
    struct NoMemory;

    impl Provider for NoMemory {
        conduit_provider::stub_descriptor!("no-memory", conduit_provider::Capability::Memory);
    }

    #[async_trait::async_trait]
    impl Memory for NoMemory {
        async fn store(&self, _record: Record) -> Result<()> {
            Ok(())
        }

        async fn search(&self, _query: Query) -> Result<Vec<Match>> {
            Ok(Vec::new())
        }

        async fn forget_conversation(&self, _conversation: ConversationId) -> Result<()> {
            Ok(())
        }
    }

    /// A detector that never activates, standing in for a real one in tests
    /// that only care whether it can be registered and enumerated.
    #[derive(Default)]
    struct NoWake;

    impl Provider for NoWake {
        conduit_provider::stub_descriptor!("no-wake", conduit_provider::Capability::Wake);
    }

    #[async_trait::async_trait]
    impl WakeWordDetector for NoWake {
        async fn detect(
            &self,
            _audio: ChunkStream<AudioChunk>,
            _phrases: Vec<WakePhrase>,
        ) -> Result<ChunkStream<Detection>> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }

    /// An identifier that never resolves a speaker, standing in for a real
    /// one in tests that only care whether it can be registered and
    /// enumerated.
    #[derive(Default)]
    struct NoSpeaker;

    impl Provider for NoSpeaker {
        conduit_provider::stub_descriptor!(
            "no-speaker",
            conduit_provider::Capability::SpeakerId
        );
    }

    #[async_trait::async_trait]
    impl SpeakerIdentifier for NoSpeaker {
        async fn identify(&self, _audio: ChunkStream<AudioChunk>) -> Result<Identification> {
            Ok(Identification { speaker: None, confidence: 0.0 })
        }

        async fn enroll(
            &self,
            _speaker: SpeakerId,
            _samples: ChunkStream<AudioChunk>,
        ) -> Result<()> {
            Ok(())
        }

        async fn forget(&self, _speaker: SpeakerId) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_empty_bundle_has_no_capabilities_to_report() {
        assert!(Providers::new().capabilities().is_empty());
    }

    #[test]
    fn wake_speaker_and_memory_register_through_the_same_path_as_every_other_capability() {
        // These three were the point of the ticket: nothing in the bundle
        // treats them differently from stt, llm, or tts.
        let providers =
            Providers::new().with_wake(NoWake).with_speaker(NoSpeaker).with_memory(NoMemory);

        assert_eq!(providers.wake().names().collect::<Vec<_>>(), ["no-wake"]);
        assert_eq!(providers.speaker().names().collect::<Vec<_>>(), ["no-speaker"]);
        assert_eq!(providers.memory().names().collect::<Vec<_>>(), ["no-memory"]);
    }

    #[test]
    fn capabilities_enumerates_only_what_was_registered() {
        let providers = Providers::new().with_wake(NoWake).with_memory(NoMemory);
        let capabilities = providers.capabilities();

        assert_eq!(
            capabilities,
            vec![
                (Capability::Memory, vec!["no-memory".to_owned()]),
                (Capability::Wake, vec!["no-wake".to_owned()]),
            ]
        );
    }

    #[test]
    fn debug_output_names_every_registered_capability_generically() {
        let providers = Providers::new().with_wake(NoWake).with_speaker(NoSpeaker);
        let rendered = format!("{providers:?}");

        assert!(rendered.contains("wake"), "{rendered}");
        assert!(rendered.contains("no-wake"), "{rendered}");
        assert!(rendered.contains("speaker_id"), "{rendered}");
        assert!(rendered.contains("no-speaker"), "{rendered}");
    }

    #[test]
    fn descriptors_report_every_capabilitys_providers_beside_their_selectors() {
        // What an operator status page enumerates. Reading it off the bundle
        // uniformly is the point: a snapshot assembled by naming stt, llm and
        // tts one at a time is a snapshot that omits these three, which is
        // exactly what it used to do.
        let providers =
            Providers::new().with_wake(NoWake).with_speaker(NoSpeaker).with_memory(NoMemory);

        let described = providers
            .descriptors()
            .into_iter()
            .map(|(capability, key, descriptor)| (capability, key, descriptor.id))
            .collect::<Vec<_>>();

        assert_eq!(
            described,
            vec![
                (Capability::Memory, "no-memory".to_owned(), "no-memory".to_owned()),
                (Capability::Wake, "no-wake".to_owned(), "no-wake".to_owned()),
                (Capability::SpeakerId, "no-speaker".to_owned(), "no-speaker".to_owned()),
            ]
        );
    }

    #[test]
    fn a_descriptor_carries_the_version_a_diagnostic_reports() {
        let providers = Providers::new().with_wake(NoWake);
        let (_, _, descriptor) =
            providers.descriptors().into_iter().next().expect("one registration");

        assert!(!descriptor.version.is_empty(), "a provider always states a version");
    }

    #[tokio::test]
    async fn health_is_asked_through_the_capability_rather_than_per_registry() {
        let providers = Providers::new().with_wake(NoWake);

        assert_eq!(
            providers.health(Capability::Wake, "no-wake").await,
            Some(conduit_provider::Health::Healthy)
        );
        // Nothing registered under that selector is a different answer from an
        // unhealthy provider: one is a name pointing at nothing, the other is a
        // provider that is there and cannot serve.
        assert_eq!(providers.health(Capability::Wake, "nope").await, None);
        assert_eq!(providers.health(Capability::Stt, "no-wake").await, None);
    }

    #[test]
    fn an_unregistered_capability_is_a_real_empty_registry_not_a_missing_slot() {
        // A bundle that never registered a recognizer must still answer
        // `stt()` with something usable, rather than panicking: a graph with
        // no `stt` node never calls it, but one that does should fail with
        // `UnknownProvider`, not a panic over an absent registry.
        let providers = Providers::new();
        assert!(providers.stt().is_empty());
        assert!(providers.stt().require("whisper").is_err());
    }
}
