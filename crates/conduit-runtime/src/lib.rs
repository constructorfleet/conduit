//! The Conduit runtime: executes a stored pipeline graph.
//!
//! [`Runner::prepare`] resolves a [`PipelineGraph`] against the registered
//! providers once, and [`Runner::run`] then executes a turn per utterance —
//! audio in, speech out, events throughout.
//!
//! The runtime executes one recognizer, one model, one synthesizer, and any
//! number of tool branches. Stages that still have no runtime contract, such
//! as memory and speaker identification, are rejected at prepare time rather
//! than silently mishandled.
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

mod emit;
pub mod plan;
pub mod sentences;
pub mod stop;
pub mod tools;
mod turn;

use std::sync::Arc;

use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::id::{ConversationId, SpeakerId};
use conduit_core::Result;
use conduit_provider::llm::LanguageModel;
use conduit_provider::stt::{AudioChunk, SpeechToText};
use conduit_provider::tool::Tool;
use conduit_provider::tts::{SpeechChunk, TextToSpeech};
use conduit_provider::{ChunkStream, Registry};
use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument;

pub use plan::Plan;
pub use stop::Stop;

/// How many synthesized chunks may be queued before synthesis waits.
///
/// The bound is the backpressure: if a device stops draining audio, the
/// runtime stops producing it rather than buffering a whole response.
const OUTPUT_BUFFER: usize = 16;

/// The providers available to a pipeline, one registry per capability.
#[derive(Default)]
pub struct Providers {
    stt: Registry<dyn SpeechToText>,
    llm: Registry<dyn LanguageModel>,
    tts: Registry<dyn TextToSpeech>,
    tools: Registry<dyn Tool>,
}

impl Providers {
    /// An empty set of providers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a recognizer under its own [`Provider::name`].
    ///
    /// [`Provider::name`]: conduit_provider::Provider::name
    #[must_use]
    pub fn with_stt<P: SpeechToText>(mut self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.stt.insert(name, Arc::new(provider));
        self
    }

    /// Registers a language model under its own name.
    #[must_use]
    pub fn with_llm<P: LanguageModel>(mut self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.llm.insert(name, Arc::new(provider));
        self
    }

    /// Registers a synthesizer under its own name.
    #[must_use]
    pub fn with_tts<P: TextToSpeech>(mut self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.tts.insert(name, Arc::new(provider));
        self
    }

    /// Registers a tool under its own name.
    ///
    /// The name a graph node refers to is the provider name; the name the
    /// model calls it by comes from the tool's own schema.
    #[must_use]
    pub fn with_tool<P: Tool>(mut self, provider: P) -> Self {
        let name = provider.name().to_owned();
        self.tools.insert(name, Arc::new(provider));
        self
    }

    /// The registered recognizers.
    #[must_use]
    pub const fn stt(&self) -> &Registry<dyn SpeechToText> {
        &self.stt
    }

    /// The registered language models.
    #[must_use]
    pub const fn llm(&self) -> &Registry<dyn LanguageModel> {
        &self.llm
    }

    /// The registered synthesizers.
    #[must_use]
    pub const fn tts(&self) -> &Registry<dyn TextToSpeech> {
        &self.tts
    }

    /// The registered tools.
    #[must_use]
    pub const fn tools(&self) -> &Registry<dyn Tool> {
        &self.tools
    }
}

/// Written by hand because the registries hold trait objects, which are not
/// `Debug`. Lists what is registered, which is what anyone printing this wants.
impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers")
            .field("stt", &self.stt.names().collect::<Vec<_>>())
            .field("llm", &self.llm.names().collect::<Vec<_>>())
            .field("tts", &self.tts.names().collect::<Vec<_>>())
            .field("tools", &self.tools.names().collect::<Vec<_>>())
            .finish()
    }
}

/// One running turn: the reply being spoken, and the id it is filed under.
pub struct Conversation {
    /// The conversation every event from this turn carries.
    pub id: ConversationId,
    /// Synthesized audio, as it is produced.
    pub audio: ChunkStream<SpeechChunk>,
    /// Asks this turn to stop talking.
    ///
    /// Distinct from dropping [`Conversation::audio`], which also ends the turn
    /// but cannot say why: a turn that notices only a failed write cannot tell
    /// an interruption from a client that vanished. Use this when a client
    /// asked, and the cancellation is reported as
    /// [`CancelReason::UserRequested`](conduit_core::event::CancelReason::UserRequested).
    pub stop: Stop,
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
}

impl Runner {
    /// Resolves `graph` against `providers` and prepares it for execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph is invalid, names an unregistered
    /// provider, is missing required node configuration, or has a topology
    /// this runtime cannot execute. See [`Plan::resolve`].
    pub fn prepare(
        graph: &PipelineGraph,
        providers: &Providers,
        bus: EventBus,
    ) -> Result<Self> {
        Ok(Self {
            plan: Arc::new(Plan::resolve(graph, providers)?),
            bus,
            format: AudioFormat::DEFAULT,
        })
    }

    /// Sets the audio format used for capture and synthesis.
    #[must_use]
    pub const fn with_format(mut self, format: AudioFormat) -> Self {
        self.format = format;
        self
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
        self.start(audio, None)
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
        self.start(audio, Some(speaker))
    }

    /// Spawns a turn, which is the only thing [`Runner::run`] and
    /// [`Runner::run_as`] differ in.
    fn start(
        &self,
        audio: ChunkStream<AudioChunk>,
        speaker: Option<SpeakerId>,
    ) -> Conversation {
        let (sender, receiver) = tokio::sync::mpsc::channel(OUTPUT_BUFFER);
        let stop = Stop::new();
        let mut turn = turn::Turn::new(
            Arc::clone(&self.plan),
            self.bus.clone(),
            self.format,
            sender,
            stop.clone(),
        );
        if let Some(speaker) = speaker {
            turn = turn.with_speaker(speaker);
        }
        let id = turn.conversation();
        let span = tracing::info_span!("conduit.turn", conversation = %id);
        tokio::spawn(turn.run(audio).instrument(span));
        Conversation { id, audio: Box::pin(ReceiverStream::new(receiver)), stop }
    }
}
