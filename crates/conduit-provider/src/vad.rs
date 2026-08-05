//! Voice activity detection provider interface.
//!
//! A detector answers one question about a stream of audio — is anyone speaking
//! right now — and answers it *about the chunks it was given*, one verdict per
//! chunk, in order. That contract is the whole reason this trait is not shaped
//! like the wake detector's, which emits an activation whenever it finds one and
//! says nothing in between.
//!
//! The reason is what the stage downstream does with the answer. A wake gate
//! only needs to know *when* to open, so a sparse stream of activations is
//! enough and a half-second pre-roll covers the lag. A trimmer has to decide,
//! for each chunk, whether that chunk goes on to the recognizer — and a chunk it
//! forwards must be the chunk it received, byte for byte, because the recognizer
//! is going to transcribe it. So verdicts are positional rather than
//! timestamped: the trimmer pairs the nth verdict with the nth chunk and never
//! has to re-derive where in the stream a decision belongs.
//!
//! Detectors are fixed-window and chunks are not — Silero scores 512 samples at
//! a time, and a device sends whatever its buffer holds — so a detector buffers
//! internally and reports each chunk as speech if any window it completed within
//! that chunk was speech. Erring toward speech is deliberate: a frame wrongly
//! called speech costs the recognizer a few milliseconds of silence, and a frame
//! wrongly called silence costs a word.

use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::descriptor::Metadata;
use crate::stt::AudioChunk;
use crate::{ChunkStream, Provider};

/// What a detector decided about one chunk of audio.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    /// Whether this chunk carries speech.
    pub speech: bool,
    /// Detector confidence that it does, in `0.0..=1.0`.
    ///
    /// Reported for a silent chunk too, and reported rather than swallowed for
    /// the same reason a rejected wake phrase is: an operator whose threshold is
    /// wrong has nothing else to tune against.
    pub confidence: f32,
}

impl Activity {
    /// A chunk carrying speech.
    #[must_use]
    pub const fn speech(confidence: f32) -> Self {
        Self { speech: true, confidence }
    }

    /// A chunk carrying none.
    #[must_use]
    pub const fn silence(confidence: f32) -> Self {
        Self { speech: false, confidence }
    }
}

/// What a pipeline asks of a detector for one utterance.
///
/// Separate from the definition because the same detector is jumpier in a
/// kitchen than in an office, and those are two pipelines rather than two
/// definitions. A field left `None` means the definition's own setting stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VadOptions {
    /// Minimum confidence to call a frame speech, in `0.0..=1.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    /// How much silence ends an utterance, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_ms: Option<u32>,
}

/// Tells speech from silence in a continuous audio stream.
#[async_trait::async_trait]
pub trait VoiceActivityDetector: Provider {
    /// Scores `audio`, emitting exactly one [`Activity`] per chunk received, in
    /// the order the chunks arrived.
    ///
    /// The one-verdict-per-chunk correlation is what lets a caller pair a
    /// decision with the audio it was about without timestamps. A detector that
    /// emitted fewer would leave a trimmer unable to tell which chunk a verdict
    /// skipped, and one that emitted more would shift every later pairing.
    ///
    /// # Errors
    ///
    /// Returns an error if the detector cannot start — a model that will not
    /// load, or a sample rate it does not score. Mid-stream failures surface as
    /// error items on the returned stream.
    async fn detect(
        &self,
        audio: ChunkStream<AudioChunk>,
        options: VadOptions,
    ) -> Result<ChunkStream<Activity>>;

    /// How much silence ends an utterance, in milliseconds, when a pipeline
    /// names none.
    ///
    /// Asked of the detector rather than fixed by the stage because the value is
    /// on the stored definition, and a definition that carried a setting nothing
    /// read would be a setting an operator can save, see accepted, and watch do
    /// nothing. A node's own `silence_ms` overrides this; the default here is the
    /// storage default, for a detector that has no opinion.
    fn silence_ms(&self) -> u32 {
        DEFAULT_SILENCE_MS
    }
}

/// The pause a detector reports when it was configured with none.
///
/// The same value the stored definition defaults to: long enough to survive the
/// pause in the middle of a sentence, short enough that someone who has finished
/// speaking is not left waiting.
pub const DEFAULT_SILENCE_MS: u32 = 700;

/// Checks that a detector advertising `metadata` scores audio at `sample_rate`.
///
/// Refused rather than resampled, and refused at registration rather than at the
/// first turn. A fixed-window detector handed the wrong rate does not degrade —
/// its window stops being the length of sound it was trained on, so it reports
/// confident nonsense — and a stage silently resampling to fix that would be
/// deciding on an operator's behalf that a rate they configured was wrong.
///
/// A detector advertising no rates accepts any: that is what "unrestricted"
/// means everywhere else in [`Metadata`], and a served detector that adapts has
/// nothing to declare.
///
/// # Errors
///
/// Returns [`conduit_core::Error::Config`] naming the rate asked for and the
/// rates available.
pub fn accepts_rate(provider: &str, metadata: &Metadata, sample_rate: u32) -> Result<()> {
    if metadata.sample_rates.is_empty() || metadata.sample_rates.contains(&sample_rate) {
        return Ok(());
    }
    Err(conduit_core::Error::Config(format!(
        "detector `{provider}` cannot score audio at {sample_rate} Hz; it scores {}",
        metadata
            .sample_rates
            .iter()
            .map(|rate| format!("{rate} Hz"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_carries_its_confidence_either_way() {
        // A silent chunk reports one too: an operator whose threshold is too
        // high sees near-misses or sees nothing at all.
        assert!(Activity::speech(0.9).speech);
        assert!(!Activity::silence(0.2).speech);
        assert!((Activity::silence(0.2).confidence - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn a_rate_the_detector_does_not_score_is_refused_by_listing_the_ones_it_does() {
        let metadata = Metadata::default().with_sample_rates(vec![8_000, 16_000]);

        accepts_rate("silero", &metadata, 16_000).expect("a rate it scores");

        let error = accepts_rate("silero", &metadata, 44_100)
            .expect_err("a rate it does not")
            .to_string();
        assert!(error.contains("44100"), "what was asked for: {error}");
        assert!(error.contains("16000"), "and what is available: {error}");
    }

    #[test]
    fn a_detector_declaring_no_rates_scores_whatever_it_is_given() {
        // The same meaning an empty list has everywhere else in the descriptor:
        // unrestricted, not none.
        accepts_rate("served", &Metadata::default(), 44_100).expect("unrestricted");
    }

    #[test]
    fn options_a_pipeline_left_alone_say_nothing_rather_than_zero() {
        // A default that serialized `threshold: 0.0` would silently make every
        // frame speech on any provider reading the field literally.
        let written = serde_json::to_value(VadOptions::default()).expect("serialize");
        assert_eq!(written, serde_json::json!({}));
    }
}
