//! LLM provider variants.

use serde::{Deserialize, Serialize};

use super::{redact_secret, ProviderSecret};

/// LLM provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmVariant {
    /// OpenAI-compatible language model.
    #[serde(rename = "openai")]
    OpenAi {
        /// Base URL including any version prefix.
        base_url: String,
        /// Optional API key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key: Option<ProviderSecret>,
        /// Advertised model ids. Empty allows any graph-selected model.
        #[serde(default)]
        models: Vec<String>,
        /// Whether the provider should stream completions when supported.
        #[serde(default)]
        streaming: bool,
        /// Optional system prompt attached at provider configuration time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
    },
}

impl LlmVariant {
    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        match self {
            Self::OpenAi { base_url, api_key, models, streaming, system_prompt } => {
                Self::OpenAi {
                    base_url: base_url.clone(),
                    api_key: redact_secret(api_key),
                    models: models.clone(),
                    streaming: *streaming,
                    system_prompt: system_prompt.clone(),
                }
            }
        }
    }
}
