//! The event vocabulary of the voice pipeline.
//!
//! Every stage publishes events rather than calling the next stage directly.
//! An [`Envelope`] carries routing and correlation metadata; the [`Event`]
//! itself carries only what happened.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::audio::AudioFormat;
use crate::graph::Modality;
use crate::id::{ConversationId, DeviceId, EventId, SpeakerId, ToolCallId, TraceId, TurnId};

/// Why a span of text was intentionally emitted as one piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UtteranceSegmentRole {
    /// Assistant text emitted before requested tools have completed.
    AssistantPreamble,
    /// Text returned by a tool and emitted directly by the runtime.
    ToolOutput,
    /// Assistant text emitted as the final answer for a turn.
    AssistantResponse,
}

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
    /// Published by a pipeline with a wake stage, at the moment the gate
    /// opens and audio starts reaching the recognizer.
    WakeWordDetected {
        /// Configured name of the phrase, e.g. `"hey jarvis"`.
        phrase: String,
        /// Detector confidence in `0.0..=1.0`.
        confidence: f32,
    },
    /// A candidate activation was scored but fell below threshold.
    ///
    /// Published rather than swallowed: a near miss is the evidence an
    /// operator has that a detector's threshold is set too high.
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
    /// Published by a pipeline with an identification stage, once per turn.
    /// A voice that matched nobody is published too, with no speaker: not
    /// recognizing someone is an answer.
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
    /// One model round requested a batch of tools.
    ToolBatchStarted {
        /// Stable batch identity for reconstruction.
        batch: String,
        /// Calls requested by this model response.
        calls: Vec<ToolCallId>,
        /// One-based model round within the turn.
        model_round: u32,
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
    /// A text span was intentionally emitted as one piece of the reply.
    ///
    /// Published whichever way the pipeline renders it. A voice pipeline
    /// follows this with audio and a text pipeline does not, so `modality` is
    /// what tells a reader which happened rather than the event's absence.
    UtteranceSegmentStarted {
        /// Stable segment identity for reconstruction.
        segment: String,
        /// Why this span is being emitted.
        role: UtteranceSegmentRole,
        /// How this span is rendered.
        modality: Modality,
        /// The text of the span.
        text: String,
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
    /// Deterministic examples for the frontend event contract.
    ///
    /// This is a contract surface, not a test convenience: the Operator
    /// Console consumes typed event fixtures generated from these examples.
    /// Keep one example for every event variant.
    #[must_use]
    pub fn contract_examples() -> Vec<Self> {
        vec![
            Self::WakeWordDetected { phrase: "hey conduit".to_owned(), confidence: 0.91 },
            Self::WakeWordRejected { phrase: "hey conduit".to_owned(), confidence: 0.12 },
            Self::AudioStarted { format: AudioFormat::DEFAULT },
            Self::AudioChunkReceived { sequence: 1, bytes: 3200 },
            Self::AudioFinished { duration_ms: 1200 },
            Self::SpeechPartial { text: "turn on".to_owned() },
            Self::SpeechFinal {
                text: "turn on the kitchen lights".to_owned(),
                confidence: Some(0.97),
                language: Some("en-US".to_owned()),
            },
            Self::SpeakerIdentified { speaker: None, confidence: 0.0 },
            Self::ConversationStarted,
            Self::TurnStarted { turn: TurnId::from_uuid(contract_uuid(10)) },
            Self::ConversationCancelled { reason: CancelReason::UserRequested },
            Self::ConversationCompleted,
            Self::LlmRequestStarted { model: "fake-llm".to_owned() },
            Self::LlmToken { delta: "The lights are on.".to_owned() },
            Self::LlmFinished {
                reason: FinishReason::Stop,
                prompt_tokens: Some(42),
                completion_tokens: Some(7),
            },
            Self::ToolRequested {
                call: ToolCallId::new("call_contract"),
                name: "lights.turn_on".to_owned(),
            },
            Self::ToolBatchStarted {
                batch: "round-1".to_owned(),
                calls: vec![ToolCallId::new("call_contract")],
                model_round: 1,
            },
            Self::ToolStarted { call: ToolCallId::new("call_contract") },
            Self::ToolConfirmationRequested {
                call: ToolCallId::new("call_contract"),
                prompt: "Turn on the kitchen lights?".to_owned(),
            },
            Self::ToolCompleted { call: ToolCallId::new("call_contract"), duration_ms: 34 },
            Self::ToolFailed {
                call: ToolCallId::new("call_contract"),
                error: "permission denied".to_owned(),
            },
            Self::TtsStarted { voice: "alloy".to_owned() },
            Self::UtteranceSegmentStarted {
                segment: "assistant-response-1".to_owned(),
                role: UtteranceSegmentRole::AssistantResponse,
                modality: Modality::Audio,
                text: "The lights are on.".to_owned(),
            },
            // The same fact from a text pipeline. Both renderings are in the
            // contract so a client cannot be written against speech alone.
            Self::UtteranceSegmentStarted {
                segment: "assistant-response-2".to_owned(),
                role: UtteranceSegmentRole::AssistantResponse,
                modality: Modality::Text,
                text: "The lights are on.".to_owned(),
            },
            Self::AudioStreaming { sequence: 2, bytes: 6400 },
            Self::TtsFinished { duration_ms: 900 },
            Self::StageFailed {
                node: "tts".to_owned(),
                error: "connection refused".to_owned(),
                recovered: false,
            },
        ]
    }

    /// Serialized event variant name used by the frontend event contract.
    #[must_use]
    pub const fn contract_type(&self) -> &'static str {
        match self {
            Self::WakeWordDetected { .. } => "WakeWordDetected",
            Self::WakeWordRejected { .. } => "WakeWordRejected",
            Self::AudioStarted { .. } => "AudioStarted",
            Self::AudioChunkReceived { .. } => "AudioChunkReceived",
            Self::AudioFinished { .. } => "AudioFinished",
            Self::SpeechPartial { .. } => "SpeechPartial",
            Self::SpeechFinal { .. } => "SpeechFinal",
            Self::SpeakerIdentified { .. } => "SpeakerIdentified",
            Self::ConversationStarted => "ConversationStarted",
            Self::TurnStarted { .. } => "TurnStarted",
            Self::ConversationCancelled { .. } => "ConversationCancelled",
            Self::ConversationCompleted => "ConversationCompleted",
            Self::LlmRequestStarted { .. } => "LlmRequestStarted",
            Self::LlmToken { .. } => "LlmToken",
            Self::LlmFinished { .. } => "LlmFinished",
            Self::ToolRequested { .. } => "ToolRequested",
            Self::ToolBatchStarted { .. } => "ToolBatchStarted",
            Self::ToolStarted { .. } => "ToolStarted",
            Self::ToolConfirmationRequested { .. } => "ToolConfirmationRequested",
            Self::ToolCompleted { .. } => "ToolCompleted",
            Self::ToolFailed { .. } => "ToolFailed",
            Self::TtsStarted { .. } => "TtsStarted",
            Self::UtteranceSegmentStarted { .. } => "UtteranceSegmentStarted",
            Self::AudioStreaming { .. } => "AudioStreaming",
            Self::TtsFinished { .. } => "TtsFinished",
            Self::StageFailed { .. } => "StageFailed",
        }
    }

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
            | Self::ToolBatchStarted { .. }
            | Self::ToolStarted { .. }
            | Self::ToolConfirmationRequested { .. }
            | Self::ToolCompleted { .. }
            | Self::ToolFailed { .. } => Stage::Tools,
            Self::TtsStarted { .. }
            | Self::UtteranceSegmentStarted { .. }
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

const fn contract_uuid(last_byte: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(last_byte)
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
    fn an_activation_and_a_match_are_filed_under_their_own_stages() {
        // A subscriber follows one stage at a time, so an event filed under
        // the wrong one is invisible to whoever asked for it. Wake word
        // detection and speaker identification were the last two stages
        // nothing published; now that the runtime does, where their events
        // land is what makes `?stage=wake_word` show anything.
        assert_eq!(
            Event::WakeWordDetected { phrase: String::new(), confidence: 0.0 }.stage(),
            Stage::WakeWord
        );
        assert_eq!(
            Event::WakeWordRejected { phrase: String::new(), confidence: 0.0 }.stage(),
            Stage::WakeWord
        );
        assert_eq!(
            Event::SpeakerIdentified { speaker: None, confidence: 0.0 }.stage(),
            Stage::Identity
        );
    }
}
