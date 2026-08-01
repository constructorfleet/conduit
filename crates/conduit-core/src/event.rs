//! The event vocabulary of the voice pipeline.
//!
//! Every stage publishes events rather than calling the next stage directly.
//! An [`Envelope`] carries routing and correlation metadata; the [`Event`]
//! itself carries only what happened.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::audio::AudioFormat;
use crate::id::{ConversationId, DeviceId, EventId, SpeakerId, ToolCallId, TraceId, TurnId};

/// An event plus the metadata needed to correlate and route it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Unique id of this event.
    pub id: EventId,
    /// Correlates every event produced by one trip through the pipeline.
    pub trace: TraceId,
    /// When the event was published.
    pub at: DateTime<Utc>,
    /// The device the event originated from, if any.
    pub device: Option<DeviceId>,
    /// The conversation the event belongs to, if any.
    pub conversation: Option<ConversationId>,
    /// The pipeline the event belongs to, if any.
    pub pipeline: Option<String>,
    /// What happened.
    pub event: Event,
}

impl Envelope {
    /// Creates an envelope stamped with a fresh id and the current time.
    #[must_use]
    pub fn new(trace: TraceId, event: Event) -> Self {
        Self {
            id: EventId::new(),
            trace,
            at: Utc::now(),
            device: None,
            conversation: None,
            pipeline: None,
            event,
        }
    }

    /// Attaches the originating device.
    #[must_use]
    pub fn with_device(mut self, device: DeviceId) -> Self {
        self.device = Some(device);
        self
    }

    /// Attaches the owning conversation.
    #[must_use]
    pub fn with_conversation(mut self, conversation: ConversationId) -> Self {
        self.conversation = Some(conversation);
        self
    }

    /// Attaches the pipeline that produced the event.
    #[must_use]
    pub fn with_pipeline(mut self, pipeline: impl Into<String>) -> Self {
        self.pipeline = Some(pipeline.into());
        self
    }
}

/// Every observable transition in the voice pipeline.
///
/// The variants are ordered to mirror the flow of a single utterance: wake
/// word, capture, transcription, identification, reasoning, tools, speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
#[non_exhaustive]
pub enum Event {
    // ---- wake word -------------------------------------------------------
    /// A wake word crossed its detection threshold.
    ///
    /// Nothing publishes this: there is no wake word provider trait, so
    /// detection happens on the device and never reaches the bus. See
    /// [`Stage::has_emitter`].
    WakeWordDetected {
        /// Configured name of the phrase, e.g. `"hey jarvis"`.
        phrase: String,
        /// Detector confidence in `0.0..=1.0`.
        confidence: f32,
    },
    /// A candidate activation was scored but fell below threshold.
    ///
    /// Nothing publishes this, for the same reason as
    /// [`Event::WakeWordDetected`].
    WakeWordRejected {
        /// Configured name of the phrase that was considered.
        phrase: String,
        /// Detector confidence in `0.0..=1.0`.
        confidence: f32,
    },

    // ---- capture ---------------------------------------------------------
    /// Audio capture began.
    AudioStarted {
        /// Format the device is streaming in.
        format: AudioFormat,
    },
    /// A chunk of captured audio was received.
    AudioChunkReceived {
        /// Monotonic index of the chunk within the stream.
        sequence: u64,
        /// Size of the chunk on the wire.
        bytes: usize,
    },
    /// Capture stopped, either from silence detection or an explicit end.
    AudioFinished {
        /// Total duration captured.
        duration_ms: u64,
    },

    // ---- transcription ---------------------------------------------------
    /// An in-progress transcript that may still change.
    SpeechPartial {
        /// Best-guess text so far.
        text: String,
    },
    /// A stable transcript for the utterance.
    SpeechFinal {
        /// The recognized text.
        text: String,
        /// Recognizer confidence in `0.0..=1.0`, when reported.
        confidence: Option<f32>,
        /// BCP-47 language tag, when detected.
        language: Option<String>,
    },

    // ---- identity --------------------------------------------------------
    /// The speaker was matched against an enrolled voice print.
    ///
    /// Nothing publishes this: no provider identifies a voice, so a turn has
    /// nothing to learn an identity from. See [`Stage::has_emitter`].
    SpeakerIdentified {
        /// The matched speaker, or `None` when the voice is unknown.
        speaker: Option<SpeakerId>,
        /// Match confidence in `0.0..=1.0`.
        confidence: f32,
    },

    // ---- conversation ----------------------------------------------------
    /// A conversation began.
    ConversationStarted,
    /// A turn within the conversation began.
    TurnStarted {
        /// The turn being started.
        turn: TurnId,
    },
    /// The conversation was cancelled, e.g. by barge-in or timeout.
    ConversationCancelled {
        /// Why the conversation ended early.
        reason: CancelReason,
    },
    /// The conversation finished normally.
    ConversationCompleted,

    // ---- reasoning -------------------------------------------------------
    /// A request was dispatched to a language model.
    LlmRequestStarted {
        /// Model identifier, e.g. `"claude-opus-5"`.
        model: String,
    },
    /// A token (or token group) was streamed back.
    LlmToken {
        /// The text delta.
        delta: String,
    },
    /// The model finished responding.
    LlmFinished {
        /// Why generation stopped.
        reason: FinishReason,
        /// Tokens consumed by the prompt, when reported.
        prompt_tokens: Option<u32>,
        /// Tokens produced, when reported.
        completion_tokens: Option<u32>,
    },

    // ---- tools -----------------------------------------------------------
    /// The model asked for a tool to be run.
    ToolRequested {
        /// Identifies this invocation across its lifecycle.
        call: ToolCallId,
        /// Registered tool name.
        name: String,
    },
    /// Execution of a requested tool began.
    ToolStarted {
        /// The invocation being started.
        call: ToolCallId,
    },
    /// A tool required a speaker's confirmation, and so was not run.
    ///
    /// Nothing collects an answer yet, so this is where the call ends: the
    /// runtime refuses it and tells the model. Read it as "a tool was blocked
    /// on a human", not as a question awaiting a reply.
    ToolConfirmationRequested {
        /// The invocation that was refused.
        call: ToolCallId,
        /// The question a speaker would have had to answer.
        prompt: String,
    },
    /// A tool returned successfully.
    ToolCompleted {
        /// The invocation that completed.
        call: ToolCallId,
        /// Wall-clock execution time.
        duration_ms: u64,
    },
    /// A tool failed.
    ToolFailed {
        /// The invocation that failed.
        call: ToolCallId,
        /// Human-readable failure description.
        error: String,
    },

    // ---- synthesis -------------------------------------------------------
    /// Speech synthesis began.
    TtsStarted {
        /// Selected voice identifier.
        voice: String,
    },
    /// Synthesized audio is being streamed to the device.
    AudioStreaming {
        /// Monotonic index of the chunk within the stream.
        sequence: u64,
        /// Size of the chunk on the wire.
        bytes: usize,
    },
    /// Synthesis and playback completed.
    TtsFinished {
        /// Total duration synthesized.
        duration_ms: u64,
    },

    // ---- diagnostics -----------------------------------------------------
    /// A stage failed in a way worth surfacing to operators.
    StageFailed {
        /// Pipeline node that failed.
        node: String,
        /// Human-readable failure description.
        error: String,
        /// Whether the pipeline recovered, e.g. by failing over.
        recovered: bool,
    },
}

impl Event {
    /// The pipeline stage this event belongs to.
    ///
    /// Used for metric labels and for filtering event subscriptions.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        match self {
            Self::WakeWordDetected { .. } | Self::WakeWordRejected { .. } => Stage::WakeWord,
            Self::AudioStarted { .. }
            | Self::AudioChunkReceived { .. }
            | Self::AudioFinished { .. } => Stage::Capture,
            Self::SpeechPartial { .. } | Self::SpeechFinal { .. } => Stage::Transcription,
            Self::SpeakerIdentified { .. } => Stage::Identity,
            Self::ConversationStarted
            | Self::TurnStarted { .. }
            | Self::ConversationCancelled { .. }
            | Self::ConversationCompleted => Stage::Conversation,
            Self::LlmRequestStarted { .. }
            | Self::LlmToken { .. }
            | Self::LlmFinished { .. } => Stage::Reasoning,
            Self::ToolRequested { .. }
            | Self::ToolStarted { .. }
            | Self::ToolConfirmationRequested { .. }
            | Self::ToolCompleted { .. }
            | Self::ToolFailed { .. } => Stage::Tools,
            Self::TtsStarted { .. }
            | Self::AudioStreaming { .. }
            | Self::TtsFinished { .. } => Stage::Synthesis,
            Self::StageFailed { .. } => Stage::Diagnostics,
        }
    }

    /// Whether this event ends the pipeline run it belongs to.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::ConversationCompleted | Self::ConversationCancelled { .. })
    }
}

/// Coarse grouping of events by pipeline stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Stage {
    /// Wake word detection.
    WakeWord,
    /// Microphone capture.
    Capture,
    /// Speech-to-text.
    Transcription,
    /// Speaker identification.
    Identity,
    /// Conversation lifecycle.
    Conversation,
    /// Language model inference.
    Reasoning,
    /// Tool execution.
    Tools,
    /// Text-to-speech and playback.
    Synthesis,
    /// Operational failures and health signals.
    Diagnostics,
}

impl Stage {
    /// Whether anything in Conduit publishes events for this stage.
    ///
    /// The vocabulary is deliberately wider than the implementation — a stage
    /// is named here before a provider exists for it, so the wire format does
    /// not change when one arrives. But a subscriber that filters on a stage
    /// nothing emits gets a stream that stays open and silent forever, which
    /// reads exactly like a broken pipeline. Whoever accepts a subscription can
    /// ask this and refuse instead, so "nothing is happening" is answered at
    /// subscribe time rather than left to be diagnosed.
    ///
    /// This is about emitters, not about whether a *particular* pipeline
    /// reaches a stage: `tools` has an emitter even though a graph with no tool
    /// nodes will never reach it.
    ///
    /// The match is exhaustive on purpose, despite `Stage` being
    /// `#[non_exhaustive]`: a stage added without deciding whether anything
    /// publishes it should fail to compile rather than default to either
    /// answer. Defaulting to "silent" would refuse a subscription to a stage
    /// that works; defaulting to "emitted" would reintroduce the silent stream
    /// this exists to prevent.
    #[must_use]
    pub const fn has_emitter(self) -> bool {
        match self {
            Self::Capture
            | Self::Transcription
            | Self::Conversation
            | Self::Reasoning
            | Self::Tools
            | Self::Synthesis
            | Self::Diagnostics => true,
            // No wake word provider trait: detection happens on the device and
            // is never reported to the server.
            Self::WakeWord => false,
            // No speaker identification provider, so nothing ever matches a
            // voice print. `Runner::run_as` takes an identity but no caller
            // has one to give.
            Self::Identity => false,
        }
    }
}

/// Why a conversation ended before completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CancelReason {
    /// The user spoke over the assistant.
    ///
    /// Reserved for voice activity detected during playback, which nothing
    /// implements, so nothing publishes this. It previously stood in for a
    /// failed write to the device — a reading that made a panel counting
    /// interruptions actually count dropped connections. That case is now
    /// [`CancelReason::Disconnected`].
    BargeIn,
    /// No input arrived before the idle timeout.
    IdleTimeout,
    /// A client or operator cancelled explicitly, e.g. a device sending
    /// [`Command::Stop`](crate::device::Command::Stop).
    UserRequested,
    /// The client stopped listening: the socket closed, or a write to it
    /// failed. Says nothing about whether anyone meant to interrupt.
    Disconnected,
    /// A stage failed unrecoverably.
    Error,
    /// The service is shutting down.
    Shutdown,
}

/// Why a model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    /// The model produced a complete response.
    Stop,
    /// Generation hit the token limit.
    Length,
    /// The model is waiting on tool results.
    ToolUse,
    /// Generation was cancelled mid-stream.
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_tag_themselves_by_variant_name() {
        let json = serde_json::to_value(Event::SpeechPartial { text: "hey".into() })
            .expect("serialize");
        assert_eq!(json["type"], "SpeechPartial");
        assert_eq!(json["text"], "hey");
    }

    #[test]
    fn envelope_round_trips() {
        let envelope = Envelope::new(TraceId::new(), Event::ConversationStarted)
            .with_device(DeviceId::new())
            .with_conversation(ConversationId::new())
            .with_pipeline("kitchen");
        let json = serde_json::to_string(&envelope).expect("serialize");
        let decoded: Envelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn terminal_events_end_the_run() {
        assert!(Event::ConversationCompleted.is_terminal());
        assert!(Event::ConversationCancelled { reason: CancelReason::BargeIn }.is_terminal());
        assert!(!Event::LlmToken { delta: "x".into() }.is_terminal());
    }

    #[test]
    fn the_stages_a_turn_produces_all_have_emitters() {
        // Each of these is published by `conduit-runtime`, which the tests in
        // `crates/conduit-runtime/tests/turn.rs` assert directly. If one of
        // those stops being emitted, this claim becomes a lie that leaves a
        // subscriber waiting on a stream that will never speak.
        for stage in [
            Stage::Capture,
            Stage::Transcription,
            Stage::Conversation,
            Stage::Reasoning,
            Stage::Tools,
            Stage::Synthesis,
            Stage::Diagnostics,
        ] {
            assert!(stage.has_emitter(), "{stage:?} is emitted by the runtime");
        }
    }

    #[test]
    fn the_stages_nothing_implements_say_so() {
        // Not aspirational: there is no wake word or speaker identification
        // provider trait, so nothing can publish these. Accepting a
        // subscription to them would hand back a permanently silent stream.
        assert!(!Stage::WakeWord.has_emitter());
        assert!(!Stage::Identity.has_emitter());
    }

    #[test]
    fn a_stage_with_no_emitter_has_no_event_the_runtime_sends() {
        // The check that keeps `has_emitter` honest in the other direction.
        // Every variant below is one the runtime publishes; none of them may
        // belong to a stage this claims is silent, or a subscriber would be
        // refused a stage that does in fact carry traffic.
        let emitted = [
            Event::AudioStarted { format: AudioFormat::DEFAULT },
            Event::AudioChunkReceived { sequence: 0, bytes: 1 },
            Event::AudioFinished { duration_ms: 1 },
            Event::SpeechPartial { text: String::new() },
            Event::SpeechFinal { text: String::new(), confidence: None, language: None },
            Event::ConversationStarted,
            Event::TurnStarted { turn: TurnId::new() },
            Event::ConversationCancelled { reason: CancelReason::Error },
            Event::ConversationCompleted,
            Event::LlmRequestStarted { model: String::new() },
            Event::LlmToken { delta: String::new() },
            Event::LlmFinished {
                reason: FinishReason::Stop,
                prompt_tokens: None,
                completion_tokens: None,
            },
            Event::ToolRequested { call: ToolCallId::new("c"), name: String::new() },
            Event::ToolStarted { call: ToolCallId::new("c") },
            Event::ToolConfirmationRequested {
                call: ToolCallId::new("c"),
                prompt: String::new(),
            },
            Event::ToolCompleted { call: ToolCallId::new("c"), duration_ms: 0 },
            Event::ToolFailed { call: ToolCallId::new("c"), error: String::new() },
            Event::TtsStarted { voice: String::new() },
            Event::AudioStreaming { sequence: 0, bytes: 0 },
            Event::TtsFinished { duration_ms: 0 },
            Event::StageFailed { node: String::new(), error: String::new(), recovered: false },
        ];

        for event in emitted {
            let stage = event.stage();
            assert!(stage.has_emitter(), "{event:?} is published, so {stage:?} is not silent");
        }
    }

    #[test]
    fn the_events_with_no_emitter_belong_to_the_silent_stages() {
        // The converse: these seven have no production emitter, and each must
        // sit in a stage marked silent so nothing subscribes to it expecting
        // traffic. `SpeechPartial` is not here — it has one.
        let silent = [
            Event::WakeWordDetected { phrase: String::new(), confidence: 0.0 },
            Event::WakeWordRejected { phrase: String::new(), confidence: 0.0 },
            Event::SpeakerIdentified { speaker: None, confidence: 0.0 },
        ];

        for event in silent {
            let stage = event.stage();
            assert!(
                !stage.has_emitter(),
                "nothing publishes {event:?}, so {stage:?} is silent"
            );
        }
    }
}
