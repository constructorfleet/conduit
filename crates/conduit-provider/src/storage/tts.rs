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
        }
    }
}
