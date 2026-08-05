//! Speech recognition provider variants.

use serde::{Deserialize, Serialize};

use super::{redact_secret, ProviderSecret};

/// Speech recognition provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SttVariant {
    /// OpenAI-compatible speech recognizer.
    #[serde(rename = "openai")]
    OpenAi {
        /// Base URL including any version prefix.
        base_url: String,
        /// Model used for transcription.
        model: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Reserved; setting it changes nothing today.
        ///
        /// Conduit posts a complete recording to `audio/transcriptions` and
        /// reads one response, so there are no partials for this to gate. The
        /// vendor does offer a streaming transcription mode, which is a
        /// different request shape and a server-sent event stream rather than a
        /// flag — wiring it is its own change. Kept rather than removed so a
        /// stored definition that carries it still loads.
        #[serde(default)]
        stream: bool,
    },
    /// Wyoming speech recognizer.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Optional model hint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Whether to emit partial transcripts as the server recognizes.
        ///
        /// Off emits none. On asks the server whether it can stream and uses
        /// partials when it says yes; a server that says no still returns a
        /// correct single final rather than failing the turn.
        #[serde(default)]
        streaming: bool,
    },
    /// ElevenLabs speech recognizer.
    ///
    /// Batch only: the vendor's realtime transcription is a websocket protocol
    /// rather than a setting, so there is no `streaming` flag to offer.
    #[serde(rename = "elevenlabs")]
    ElevenLabs {
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Transcription model, e.g. `scribe_v2`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Google Cloud Speech-to-Text.
    ///
    /// No credential slot: Google credentials are discovered rather than typed,
    /// so there is nothing for an operator to paste. See `conduit-google`.
    Google {
        /// BCP-47 language tag to listen for, e.g. `en-US`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// Recognition model, e.g. `latest_long`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

impl SttVariant {
    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        match self {
            Self::OpenAi { base_url, model, api_key, stream } => Self::OpenAi {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: redact_secret(api_key),
                stream: *stream,
            },
            Self::Wyoming { url, model, streaming } => {
                Self::Wyoming { url: url.clone(), model: model.clone(), streaming: *streaming }
            }
            Self::ElevenLabs { api_key, model } => {
                Self::ElevenLabs { api_key: redact_secret(api_key), model: model.clone() }
            }
            Self::Google { language, model } => {
                Self::Google { language: language.clone(), model: model.clone() }
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
            (SttVariant::ElevenLabs { api_key: None, model: None }, "elevenlabs"),
            (SttVariant::Google { language: None, model: None }, "google"),
        ];

        for (variant, expected) in spellings {
            let encoded = serde_json::to_value(&variant).expect("serializes");
            assert_eq!(encoded.get("type").and_then(serde_json::Value::as_str), Some(expected));
            let decoded: SttVariant =
                serde_json::from_value(encoded).expect("deserializes as it serialized");
            assert_eq!(decoded, variant);
        }
    }

    #[test]
    fn an_elevenlabs_key_is_redacted_and_the_model_survives() {
        let variant = SttVariant::ElevenLabs {
            api_key: Some(ProviderSecret::Inline { value: "xi-secret".to_owned() }),
            model: Some("scribe_v2".to_owned()),
        };

        let printed = serde_json::to_string(&variant.redacted()).expect("serializes");
        assert!(!printed.contains("xi-secret"), "{printed}");
        assert!(printed.contains("scribe_v2"), "{printed}");
    }

    #[test]
    fn a_google_recognizer_survives_redaction_whole() {
        // Google discovers its credentials, so it has no secret — and redaction
        // must not quietly drop the settings it does carry.
        let google = SttVariant::Google {
            language: Some("en-GB".to_owned()),
            model: Some("latest_long".to_owned()),
        };
        assert_eq!(google.redacted(), google);
    }
}
