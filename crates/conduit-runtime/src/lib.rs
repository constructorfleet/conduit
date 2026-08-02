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

pub mod deadline;
mod emit;
pub mod plan;
pub mod sentences;
pub mod stop;
pub mod tools;
mod turn;

use std::sync::Arc;
use std::time::Duration;

use conduit_core::audio::AudioFormat;
use conduit_core::bus::EventBus;
use conduit_core::graph::PipelineGraph;
use conduit_core::id::{ConversationId, DeviceId, SpeakerId};
use conduit_core::Result;
use conduit_provider::llm::LanguageModel;
use conduit_provider::stt::{AudioChunk, SpeechToText};
use conduit_provider::tool::Tool;
use conduit_provider::tts::{SpeechChunk, TextToSpeech};
use conduit_provider::{ChunkStream, Registry};
use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument;

pub use deadline::DEFAULT_IDLE_TIMEOUT;
pub use plan::Plan;
pub use stop::Stop;
pub use turn::TurnInput;

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

    /// Sets the audio format used for capture and synthesis.
    pub fn with_format(mut self, format: AudioFormat) -> Result<Self> {
        // A pipeline fed by text has no recognizer to ask, and the format
        // still describes the audio it produces.
        if let Some(stt) = &self.plan.stt {
            if !stt.provider.supports_encoding(format.encoding) {
                return Err(conduit_core::Error::Config(format!(
                    "node `{}` uses provider `{}`, which cannot accept {:?} audio",
                    stt.node,
                    stt.provider.name(),
                    format.encoding
                )));
            }
        }
        if !self.plan.tts.supports_encoding(format.encoding) {
            return Err(conduit_core::Error::Config(format!(
                "node `{}` uses provider `{}`, which cannot produce {:?} audio",
                self.plan.tts_node,
                self.plan.tts.name(),
                format.encoding
            )));
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
        let mut turn = turn::Turn::new(
            Arc::clone(&self.plan),
            self.bus.clone(),
            self.format,
            sender,
            stop.clone(),
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
        Conversation { id, audio: Box::pin(ReceiverStream::new(receiver)), stop, turn: running }
    }
}
