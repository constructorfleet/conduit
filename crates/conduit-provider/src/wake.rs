//! Wake word provider interface.

use std::pin::Pin;
use std::task::{Context, Poll};

use conduit_core::Result;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::descriptor::{Descriptor, Metadata};
use crate::registry::Capability;
use crate::stt::AudioChunk;
use crate::{ChunkStream, Health, Provider};

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
}

/// The detector for a satellite that wakes itself.
///
/// A device running microWakeWord scores audio locally and only opens a stream
/// once it has activated, so by the time the server sees a sample the decision
/// has already been made. This detector says exactly that: it accepts
/// immediately, at full confidence, and never looks at the audio.
///
/// It is a provider rather than a special case in the runtime because the
/// difference between waking on the satellite and waking on a Wyoming server
/// is a deployment choice. Expressing it as a provider is what lets a pipeline
/// name the stage either way and everything downstream — validation, the
/// editor, the event stream — stay the same.
#[derive(Debug, Clone)]
pub struct DeviceWake {
    /// Identity, version, and the phrases the satellite is flashed with.
    descriptor: Descriptor,
}

impl DeviceWake {
    /// A detector standing for the satellite's own, listening for `phrases`.
    #[must_use]
    pub fn new(name: impl Into<String>, phrases: Vec<String>) -> Self {
        let metadata =
            Metadata::default().with_phrases(phrases.iter().map(WakePhrase::new).collect());
        Self { descriptor: Descriptor::new(name, Capability::Wake).with_metadata(metadata) }
    }

    /// Sets the human-readable name operator screens show.
    ///
    /// Separate from the identity this provider was built with: the identity
    /// is what a pipeline selects and what appears in metric labels, and this
    /// is only what a person reads.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.with_label(label);
        self
    }

    /// The phrases the satellite is flashed with.
    fn phrases(&self) -> &[WakePhrase] {
        &self.descriptor.metadata.phrases
    }
}

#[async_trait::async_trait]
impl Provider for DeviceWake {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Always healthy: there is no service to reach. A satellite that stopped
    /// detecting stops streaming, which is not something the server can probe.
    async fn health(&self) -> Health {
        Health::Healthy
    }
}

#[async_trait::async_trait]
impl WakeWordDetector for DeviceWake {
    async fn detect(
        &self,
        audio: ChunkStream<AudioChunk>,
        phrases: Vec<WakePhrase>,
    ) -> Result<ChunkStream<Detection>> {
        // Whichever phrase the pipeline asked for, reported so the event names
        // something an operator configured rather than an empty string. A
        // pipeline that named none has only the definition's list to go on.
        let phrase = phrases
            .first()
            .or_else(|| self.phrases().first())
            .map(|phrase| phrase.phrase.clone())
            .unwrap_or_else(|| self.descriptor.id.clone());
        Ok(Box::pin(AlreadyAwake {
            detection: Some(Detection { phrase, confidence: 1.0, accepted: true }),
            audio,
        }))
    }
}

/// A stream that reports one activation and then ends.
///
/// It holds the audio it was handed without reading it, so that whoever is
/// feeding the detector sees a live consumer rather than a closed one: a gate
/// whose sends started failing would report a broken detector for a stage that
/// worked exactly as intended.
struct AlreadyAwake {
    detection: Option<Detection>,
    #[allow(dead_code)]
    audio: ChunkStream<AudioChunk>,
}

impl Stream for AlreadyAwake {
    type Item = Result<Detection>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().detection.take().map(Ok))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    fn silence() -> ChunkStream<AudioChunk> {
        Box::pin(futures_util::stream::empty())
    }

    #[tokio::test]
    async fn a_satellite_that_woke_itself_is_already_awake() {
        // The device only streams once it has activated, so the first sample
        // the server sees is already past the wake word. Scoring it would
        // discard the activation the satellite already made.
        let detector = DeviceWake::new("okay-nabu", vec!["okay nabu".to_owned()]);
        let mut detections = detector
            .detect(silence(), vec![WakePhrase::new("okay nabu")])
            .await
            .expect("session");

        let first = detections.next().await.expect("a detection").expect("not an error");
        assert!(first.accepted);
        assert_eq!(first.phrase, "okay nabu");
        assert!((first.confidence - 1.0).abs() < f32::EPSILON);
        assert!(detections.next().await.is_none(), "one activation per stream");
    }

    #[tokio::test]
    async fn the_detection_names_a_phrase_the_operator_configured() {
        // A pipeline that named no phrase still gets an event naming something
        // recognizable, rather than an empty string in the event stream.
        let detector = DeviceWake::new("okay-nabu", vec!["okay nabu".to_owned()]);
        let mut detections = detector.detect(silence(), Vec::new()).await.expect("session");

        let first = detections.next().await.expect("a detection").expect("not an error");
        assert_eq!(first.phrase, "okay nabu", "the definition's phrase stands in");
    }

    #[test]
    fn a_phrase_carries_the_conventional_threshold_until_it_is_tuned() {
        assert!((WakePhrase::new("hey jarvis").threshold - 0.5).abs() < f32::EPSILON);
        assert!(
            (WakePhrase::new("hey jarvis").with_threshold(0.8).threshold - 0.8).abs()
                < f32::EPSILON
        );
    }
}
