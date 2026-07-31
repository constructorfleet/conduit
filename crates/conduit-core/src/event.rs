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
    WakeWordDetected {
        /// Configured name of the phrase, e.g. `"hey jarvis"`.
        phrase: String,
        /// Detector confidence in `0.0..=1.0`.
        confidence: f32,
    },
    /// A candidate activation was scored but fell below threshold.
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
    /// A tool asked the speaker to confirm before it may run.
    ToolConfirmationRequested {
        /// The invocation waiting for confirmation.
        call: ToolCallId,
        /// Question spoken to the speaker.
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

/// Why a conversation ended before completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CancelReason {
    /// The user spoke over the assistant.
    BargeIn,
    /// No input arrived before the idle timeout.
    IdleTimeout,
    /// A client or operator cancelled explicitly.
    UserRequested,
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
            .with_conversation(ConversationId::new());
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
}
