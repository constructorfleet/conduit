//! Speech-to-text provider interface.

use bytes::Bytes;
use conduit_core::audio::AudioFormat;
use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::descriptor::Settings;
use crate::{ChunkStream, Provider};

/// A chunk of captured audio handed to a recognizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioChunk {
    /// Monotonic index within the stream, starting at zero.
    pub sequence: u64,
    /// Encoded samples.
    pub data: Bytes,
}

/// One recognizer output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// Recognized text. For partials this replaces, rather than appends to,
    /// the previous partial.
    pub text: String,
    /// Whether the text is stable. Partials may still change; finals may not.
    pub is_final: bool,
    /// Recognizer confidence in `0.0..=1.0`, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Detected BCP-47 language tag, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Offset of this segment from the start of capture, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
}

impl Transcript {
    /// A stable transcript.
    #[must_use]
    pub fn final_text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_final: true,
            confidence: None,
            language: None,
            start_ms: None,
        }
    }

    /// An in-progress transcript that may still change.
    #[must_use]
    pub fn partial(text: impl Into<String>) -> Self {
        Self { is_final: false, ..Self::final_text(text) }
    }
}

/// Options for a transcription session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscribeOptions {
    /// Format of the incoming audio.
    pub format: AudioFormat,
    /// BCP-47 language hint. `None` asks the provider to detect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Whether to emit partial transcripts. Providers that cannot stream
    /// ignore this and emit a single final.
    pub partials: bool,
    /// Provider-specific settings, checked against the schema the provider's
    /// [`Descriptor`](crate::Descriptor) declares.
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    pub settings: Settings,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self {
            format: AudioFormat::DEFAULT,
            language: None,
            partials: true,
            settings: Settings::empty(),
        }
    }
}

/// Converts speech to text.
#[async_trait::async_trait]
pub trait SpeechToText: Provider {
    /// Starts a transcription session over `audio`.
    ///
    /// The returned stream yields partials (when requested and supported)
    /// followed by at least one final transcript, then ends when `audio`
    /// ends. Dropping the returned stream cancels the session.
    ///
    /// # Errors
    ///
    /// Returns an error if the session cannot be started. Failures that occur
    /// mid-session surface as error items on the returned stream.
    async fn transcribe(
        &self,
        audio: ChunkStream<AudioChunk>,
        options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>>;
}
