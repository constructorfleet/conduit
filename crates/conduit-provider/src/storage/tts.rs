//! Speech synthesis provider variants.

use serde::{Deserialize, Serialize};

use super::{redact_secret, ProviderSecret};

/// Speech synthesis provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TtsVariant {
    /// OpenAI-compatible speech synthesizer.
    #[serde(rename = "openai")]
    OpenAi {
        /// Base URL including any version prefix.
        base_url: String,
        /// Speech model.
        model: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Optional voice catalogue.
        #[serde(default)]
        voices: Vec<String>,
    },
    /// Wyoming speech synthesizer.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Canonical voice id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        /// Whether streaming is enabled.
        #[serde(default)]
        streaming: bool,
    },
    /// ElevenLabs speech synthesizer.
    #[serde(rename = "elevenlabs")]
    ElevenLabs {
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Synthesis model, e.g. `eleven_multilingual_v2`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Voice id to speak with.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
    },
    /// Google Cloud Text-to-Speech.
    ///
    /// No credential slot: Google credentials are discovered rather than typed,
    /// so there is nothing for an operator to paste. See `conduit-google`.
    Google {
        /// BCP-47 language tag, e.g. `en-US`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// Voice name, e.g. `en-US-Neural2-F`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
    },
    /// MaryTTS speech synthesizer.
    #[serde(rename = "marytts")]
    MaryTts {
        /// Base URL of the MaryTTS server.
        url: String,
        /// Voice name the server offers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        /// Locale to synthesize in, e.g. `en_US`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locale: Option<String>,
    },
}

impl TtsVariant {
    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        match self {
            Self::OpenAi { base_url, model, api_key, voices } => Self::OpenAi {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: redact_secret(api_key),
                voices: voices.clone(),
            },
            Self::Wyoming { url, voice, streaming } => {
                Self::Wyoming { url: url.clone(), voice: voice.clone(), streaming: *streaming }
            }
            Self::ElevenLabs { api_key, model, voice } => Self::ElevenLabs {
                api_key: redact_secret(api_key),
                model: model.clone(),
                voice: voice.clone(),
            },
            Self::Google { language, voice } => {
                Self::Google { language: language.clone(), voice: voice.clone() }
            }
            Self::MaryTts { url, voice, locale } => {
                Self::MaryTts { url: url.clone(), voice: voice.clone(), locale: locale.clone() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_is_spelled_the_way_an_operator_writes_it() {
        // The inner `type` is what a stored definition carries, so a rename here
        // silently orphans every definition an operator has already saved.
        let spellings = [
            (TtsVariant::ElevenLabs { api_key: None, model: None, voice: None }, "elevenlabs"),
            (TtsVariant::Google { language: None, voice: None }, "google"),
            (
                TtsVariant::MaryTts {
                    url: "http://localhost:59125".to_owned(),
                    voice: None,
                    locale: None,
                },
                "marytts",
            ),
        ];

        for (variant, expected) in spellings {
            let encoded = serde_json::to_value(&variant).expect("serializes");
            assert_eq!(encoded.get("type").and_then(serde_json::Value::as_str), Some(expected));
            let decoded: TtsVariant =
                serde_json::from_value(encoded).expect("deserializes as it serialized");
            assert_eq!(decoded, variant);
        }
    }

    #[test]
    fn an_elevenlabs_key_is_redacted_and_the_rest_of_the_definition_survives() {
        let variant = TtsVariant::ElevenLabs {
            api_key: Some(ProviderSecret::Inline { value: "xi-secret".to_owned() }),
            model: Some("eleven_multilingual_v2".to_owned()),
            voice: Some("21m00Tcm4TlvDq8ikWAM".to_owned()),
        };

        let redacted = variant.redacted();
        let printed = serde_json::to_string(&redacted).expect("serializes");
        assert!(!printed.contains("xi-secret"), "{printed}");
        assert!(printed.contains("eleven_multilingual_v2"), "{printed}");
    }

    #[test]
    fn the_providers_with_nothing_to_redact_survive_redaction_whole() {
        // Google discovers its credentials and MaryTTS is on the LAN, so
        // neither has a secret — and redaction must not quietly drop the
        // settings they do carry.
        let google = TtsVariant::Google { language: Some("en-GB".to_owned()), voice: None };
        assert_eq!(google.redacted(), google);

        let mary = TtsVariant::MaryTts {
            url: "http://voice.lan:59125".to_owned(),
            voice: Some("cmu-slt-hsmm".to_owned()),
            locale: Some("en_US".to_owned()),
        };
        assert_eq!(mary.redacted(), mary);
    }
}
