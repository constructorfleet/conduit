//! Wake word provider interface.

use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::stt::AudioChunk;
use crate::{ChunkStream, Provider};

/// A wake phrase the detector is listening for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakePhrase {
    /// Configured name of the phrase, e.g. `"hey jarvis"`.
    pub phrase: String,
    /// Minimum confidence to accept, in `0.0..=1.0`. Lower is more sensitive.
    pub threshold: f32,
}

impl WakePhrase {
    /// A phrase with the conventional 0.5 threshold.
    #[must_use]
    pub fn new(phrase: impl Into<String>) -> Self {
        Self { phrase: phrase.into(), threshold: 0.5 }
    }

    /// Sets the acceptance threshold.
    #[must_use]
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }
}

/// The result of scoring a candidate activation.
///
/// Rejections are reported rather than swallowed: near-misses are how
/// operators tune sensitivity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    /// Which configured phrase was scored.
    pub phrase: String,
    /// Detector confidence in `0.0..=1.0`.
    pub confidence: f32,
    /// Whether the score met the phrase's threshold.
    pub accepted: bool,
}

/// Listens for wake phrases in a continuous audio stream.
#[async_trait::async_trait]
pub trait WakeWordDetector: Provider {
    /// Scores `audio` against `phrases`, emitting one [`Detection`] per
    /// candidate activation.
    ///
    /// The stream runs until `audio` ends; a detection does not terminate it,
    /// because the same microphone keeps listening for the next activation.
    ///
    /// # Errors
    ///
    /// Returns an error if a phrase is unsupported or the detector cannot
    /// start. Mid-stream failures surface as error items on the stream.
    async fn detect(
        &self,
        audio: ChunkStream<AudioChunk>,
        phrases: Vec<WakePhrase>,
    ) -> Result<ChunkStream<Detection>>;

    /// Phrases this detector has models for. Empty means the detector trains
    /// or loads phrases on demand.
    fn available_phrases(&self) -> &[String] {
        &[]
    }
}
