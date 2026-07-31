//! Text-to-speech provider interface.

use bytes::Bytes;
use conduit_core::audio::AudioFormat;
use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::{ChunkStream, Provider};

/// A voice a provider can speak with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voice {
    /// Provider-scoped identifier used in requests.
    pub id: String,
    /// Human-readable name for the UI.
    pub name: String,
    /// BCP-47 language tag this voice speaks.
    pub language: String,
}

/// A request for synthesized speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SynthesisRequest {
    /// Text to speak. Providers that accept SSML detect it themselves.
    pub text: String,
    /// Voice identifier, or `None` for the provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Desired output format. Providers that cannot honour it report the
    /// format they actually produced on the first chunk.
    pub format: AudioFormat,
    /// Speaking rate multiplier, where `1.0` is the voice's natural rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
    /// Provider-specific settings, e.g. emotion or style controls.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

impl SynthesisRequest {
    /// A request for `text` in the provider's default voice and the
    /// pipeline's interchange format.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            voice: None,
            format: AudioFormat::DEFAULT,
            rate: None,
            extra: serde_json::Value::Null,
        }
    }
}

/// A chunk of synthesized audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechChunk {
    /// Monotonic index within the stream, starting at zero.
    pub sequence: u64,
    /// Format of `data`. Constant for the lifetime of one stream.
    pub format: AudioFormat,
    /// Encoded samples.
    pub data: Bytes,
}

/// Converts text to speech.
#[async_trait::async_trait]
pub trait TextToSpeech: Provider {
    /// Streams synthesized audio for `request`.
    ///
    /// Chunks are emitted as they are produced so playback can begin before
    /// synthesis completes. Dropping the stream stops synthesis, which is how
    /// barge-in silences the assistant mid-sentence.
    ///
    /// # Errors
    ///
    /// Returns an error if the request is rejected outright. Mid-stream
    /// failures surface as error items on the returned stream.
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>>;

    /// Voices this provider offers.
    ///
    /// # Errors
    ///
    /// Returns an error if the catalogue cannot be retrieved.
    async fn voices(&self) -> Result<Vec<Voice>>;
}
