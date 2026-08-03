//! Speaker identification provider variants.

use serde::{Deserialize, Serialize};

use super::{default_threshold_percent, redact_secret, ProviderSecret, SpeakerEngine};

/// Speaker identification provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpeakerIdVariant {
    /// Speaker identification on a Diarization_Server instance.
    ///
    /// A separate variant rather than a flag on [`Self::Http`] because the two
    /// speak different dialects — raw samples and query parameters against a
    /// container and paths — and a definition should say which service it is
    /// describing rather than which options that service happens to want.
    DiarizationServer {
        /// Base URL of the Diarization_Server instance.
        base_url: String,
        /// Minimum similarity to call a voice a match, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    /// Speaker identification over the Conduit speaker HTTP contract.
    Http {
        /// Base URL of the identification service.
        base_url: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Which embedding model is behind the endpoint.
        engine: SpeakerEngine,
        /// Minimum similarity to call a voice a match, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
}

impl SpeakerIdVariant {
    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        match self {
            Self::DiarizationServer { base_url, threshold_percent } => {
                Self::DiarizationServer {
                    base_url: base_url.clone(),
                    threshold_percent: *threshold_percent,
                }
            }
            Self::Http { base_url, api_key, engine, threshold_percent } => Self::Http {
                base_url: base_url.clone(),
                api_key: redact_secret(api_key),
                engine: *engine,
                threshold_percent: *threshold_percent,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ProviderDefinitionVariant, ProviderSecret};
    use super::*;

    #[test]
    fn a_speaker_definitions_key_is_redacted_and_survives_an_update_that_omits_it() {
        // The same secret semantics every keyed definition has: a read never
        // shows the key, and saving what a read returned must not erase it.
        let stored = ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::Http {
                base_url: "https://voices.example".to_owned(),
                api_key: Some(ProviderSecret::Inline { value: "sk-live".to_owned() }),
                engine: SpeakerEngine::SpeechBrain,
                threshold_percent: 70,
            },
        };

        let read = stored.redacted();
        assert_eq!(
            read,
            ProviderDefinitionVariant::SpeakerId {
                variant: SpeakerIdVariant::Http {
                    base_url: "https://voices.example".to_owned(),
                    api_key: Some(ProviderSecret::Redacted),
                    engine: SpeakerEngine::SpeechBrain,
                    threshold_percent: 70,
                }
            }
        );

        let saved = read.with_secret_updates_from(Some(&stored));
        assert_eq!(saved, stored, "saving a redacted key keeps the stored one");
    }
}
