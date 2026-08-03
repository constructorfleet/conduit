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
        /// Whether the recognizer streams partials.
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
        /// Whether streaming is enabled.
        #[serde(default)]
        streaming: bool,
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
        }
    }
}
