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
    /// Anthropic's Messages API.
    ///
    /// A separate variant rather than a base URL under [`Self::OpenAi`]
    /// because it is a different wire format, not a different host: the
    /// credential travels in `x-api-key` rather than as a bearer token, the
    /// request pins `anthropic-version`, the system prompt is a top-level
    /// field instead of a message, and the response is a block-structured
    /// event stream rather than chat completion chunks.
    #[serde(rename = "anthropic")]
    Anthropic {
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
            Self::Anthropic { base_url, api_key, models, streaming, system_prompt } => {
                Self::Anthropic {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic() -> LlmVariant {
        LlmVariant::Anthropic {
            base_url: "https://api.anthropic.com/v1".to_owned(),
            api_key: Some(ProviderSecret::Inline { value: "sk-ant-test".to_owned() }),
            models: vec!["claude-opus-5".to_owned()],
            streaming: true,
            system_prompt: Some("Be terse.".to_owned()),
        }
    }

    #[test]
    fn an_anthropic_definition_round_trips_under_its_own_tag() {
        let value = serde_json::to_value(anthropic()).expect("serialize");
        assert_eq!(
            value.get("type"),
            Some(&serde_json::json!("anthropic")),
            "the inner tag names the vendor, not the capability: {value}"
        );

        let back: LlmVariant = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, anthropic());
    }

    #[test]
    fn an_anthropic_key_is_redacted_rather_than_echoed() {
        let LlmVariant::Anthropic { api_key, models, .. } = anthropic().redacted() else {
            panic!("redaction keeps the variant");
        };
        assert_eq!(api_key, Some(ProviderSecret::Redacted), "the key never leaves the process");
        assert_eq!(models, vec!["claude-opus-5".to_owned()], "everything else survives");
    }
}
