//! Speaker identification provider interface.

use conduit_core::id::SpeakerId;
use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::stt::AudioChunk;
use crate::{ChunkStream, Provider};

/// The outcome of matching a voice against enrolled speakers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identification {
    /// The matched speaker, or `None` when no enrolled voice print was close
    /// enough. Unknown speakers are a normal outcome, not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<SpeakerId>,
    /// Match confidence in `0.0..=1.0`.
    pub confidence: f32,
}

impl Identification {
    /// An unmatched voice.
    #[must_use]
    pub const fn unknown(confidence: f32) -> Self {
        Self { speaker: None, confidence }
    }
}

/// Recognizes who is speaking.
#[async_trait::async_trait]
pub trait SpeakerIdentifier: Provider {
    /// Identifies the speaker in `audio`.
    ///
    /// Consumes the utterance and returns one result; identification is not
    /// streamed because a partial answer is not actionable.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio cannot be processed. A voice that
    /// matches nobody is reported as [`Identification::unknown`].
    async fn identify(&self, audio: ChunkStream<AudioChunk>) -> Result<Identification>;

    /// Enrolls a new voice print from one or more sample utterances.
    ///
    /// # Errors
    ///
    /// Returns an error if the samples are too short or too noisy to build a
    /// usable voice print.
    async fn enroll(&self, speaker: SpeakerId, samples: ChunkStream<AudioChunk>) -> Result<()>;

    /// Removes a speaker's voice print.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable. Removing an unknown
    /// speaker succeeds.
    async fn forget(&self, speaker: SpeakerId) -> Result<()>;
}
