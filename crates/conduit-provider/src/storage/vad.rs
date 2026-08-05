//! Voice activity detection provider variants.

use serde::{Deserialize, Serialize};

use super::default_threshold_percent;

/// How much silence ends an utterance, in milliseconds, when a definition says
/// nothing.
///
/// Long enough to survive the pause in the middle of a sentence — "turn on the
/// lights ... in the kitchen" — and short enough that a person who has finished
/// speaking is not left waiting. Shorter values cut people off mid-thought,
/// which reaches an operator as a recognizer that truncates every long request.
#[must_use]
pub const fn default_silence_ms() -> u32 {
    crate::vad::DEFAULT_SILENCE_MS
}

/// Voice activity detection provider variants.
///
/// One variant, and that is a decision rather than a first instalment. A
/// detector answers a yes-or-no question about a frame of audio, so unlike
/// recognition or synthesis there is nothing for competing vendors to differ
/// *about* that an operator would choose between — what differs is accuracy in
/// noise, which is the model's business rather than the definition's. The
/// reasoning for the detectors Conduit does not reach is recorded in the
/// project README beside the other declined providers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum VadVariant {
    /// Silero, scored in the Conduit process from an ONNX model on disk.
    ///
    /// No base URL and no API key: there is no service. This is the reason
    /// [`default_silence_ms`] and the threshold live on the definition at all —
    /// with nothing to configure about *reaching* the detector, what is left is
    /// what the detector decides.
    Silero {
        /// Path the ONNX model is read from.
        ///
        /// Unset means the conventional `vad-models` directory under the data
        /// directory, which is where an operator who followed the compose file
        /// put it — the same convention the local wake runtime uses.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_path: Option<String>,
        /// Minimum confidence to call a frame speech, as a percentage.
        ///
        /// A default here and an override on the node, because the same
        /// detector is jumpier in a kitchen than in an office and the two rooms
        /// are two pipelines rather than two definitions.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
        /// How much silence ends an utterance, in milliseconds.
        #[serde(default = "default_silence_ms")]
        silence_ms: u32,
    },
}

impl VadVariant {
    /// Returns a copy with inline secrets redacted.
    ///
    /// Nothing is redacted, because there is nothing to redact: a detector
    /// reading a file off local disk has no credential. Written as a whole
    /// clone rather than as `self.clone()` so that a variant added later with a
    /// key cannot inherit a redaction that silently does nothing — the same
    /// reason the keyless synthesis variants spell theirs out.
    pub(super) fn redacted(&self) -> Self {
        match self {
            Self::Silero { model_path, threshold_percent, silence_ms } => Self::Silero {
                model_path: model_path.clone(),
                threshold_percent: *threshold_percent,
                silence_ms: *silence_ms,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ProviderDefinitionVariant;
    use super::*;

    #[test]
    fn a_detector_is_written_as_the_word_the_project_is_named_by() {
        let written = serde_json::to_value(VadVariant::Silero {
            model_path: None,
            threshold_percent: 50,
            silence_ms: 700,
        })
        .expect("serialize");

        assert_eq!(written["type"], "silero");
        assert!(written.get("model_path").is_none(), "the conventional directory is unwritten");
    }

    #[test]
    fn a_definition_naming_only_the_detector_still_names_a_threshold_and_a_pause() {
        // The common definition: an operator who dropped the model where the
        // compose file said to has nothing else to say.
        let variant: VadVariant =
            serde_json::from_value(serde_json::json!({ "type": "silero" })).expect("parse");

        let VadVariant::Silero { model_path, threshold_percent, silence_ms } = variant;
        assert!(model_path.is_none());
        assert_eq!(threshold_percent, default_threshold_percent());
        assert_eq!(silence_ms, default_silence_ms());
    }

    #[test]
    fn a_keyless_detector_reads_back_whole() {
        // Unlike every keyed capability, a read of this definition is the
        // definition: there is no credential for redaction to remove, so an
        // operator saving what they read cannot lose anything.
        let stored = ProviderDefinitionVariant::Vad {
            variant: VadVariant::Silero {
                model_path: Some("/models/silero_vad.onnx".to_owned()),
                threshold_percent: 70,
                silence_ms: 400,
            },
        };

        assert_eq!(stored.redacted(), stored, "nothing to hide, so nothing hidden");
    }
}
