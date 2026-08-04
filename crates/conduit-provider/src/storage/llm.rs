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
    /// Amazon Bedrock's Converse API.
    ///
    /// Named by region rather than by URL, because that is what a Bedrock
    /// definition actually knows: the SDK resolves the endpoint, and storing a
    /// base URL beside it would be recording something nothing reads. The
    /// credential is optional for the same reason it is on nothing else here —
    /// the usual deployment has none to name, because a task role, an instance
    /// profile, or a shared config file already supplies one.
    #[serde(rename = "bedrock")]
    Bedrock {
        /// AWS region the model is invoked in, e.g. `us-west-2`.
        ///
        /// Also part of the model id: an inference profile is prefixed with the
        /// geography it routes within (`us.`, `eu.`), and the two must agree.
        region: String,
        /// Named profile from the shared AWS config file to load credentials
        /// from.
        ///
        /// `None` uses the default chain — environment, task role, instance
        /// profile, default profile — which is what a deployment given its own
        /// role wants.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        /// A Bedrock API key, sent as a bearer token.
        ///
        /// The long-lived alternative to signing with credentials, for a
        /// deployment that has no AWS identity to give this process. `None`
        /// signs with whatever the credential chain resolves.
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
            Self::Bedrock { region, profile, api_key, models, streaming, system_prompt } => {
                Self::Bedrock {
                    region: region.clone(),
                    profile: profile.clone(),
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

    fn bedrock() -> LlmVariant {
        LlmVariant::Bedrock {
            region: "us-west-2".to_owned(),
            profile: Some("voice".to_owned()),
            api_key: Some(ProviderSecret::Inline { value: "bedrock-api-key".to_owned() }),
            models: vec!["us.anthropic.claude-opus-5".to_owned()],
            streaming: true,
            system_prompt: None,
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
    fn a_bedrock_definition_names_a_region_rather_than_a_url() {
        // Where the endpoint comes from is the difference between this variant
        // and the other two: the SDK builds it from the region, so a definition
        // that carried a base URL would be storing something nothing reads.
        let value = serde_json::to_value(bedrock()).expect("serialize");

        assert_eq!(value.get("type"), Some(&serde_json::json!("bedrock")));
        assert_eq!(value.get("region"), Some(&serde_json::json!("us-west-2")));
        assert!(value.get("base_url").is_none(), "the region is the endpoint: {value}");

        let back: LlmVariant = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, bedrock());
    }

    #[test]
    fn a_bedrock_definition_may_leave_every_credential_to_the_environment() {
        // The usual deployment: a task role, an instance profile, or whatever
        // `aws configure` wrote. A definition that had to name a key could not
        // describe it.
        let variant: LlmVariant = serde_json::from_value(serde_json::json!({
            "type": "bedrock",
            "region": "us-east-1",
        }))
        .expect("deserialize");

        assert_eq!(
            variant,
            LlmVariant::Bedrock {
                region: "us-east-1".to_owned(),
                profile: None,
                api_key: None,
                models: Vec::new(),
                streaming: false,
                system_prompt: None,
            }
        );
    }

    #[test]
    fn a_bedrock_key_is_redacted_rather_than_echoed() {
        let LlmVariant::Bedrock { api_key, region, profile, .. } = bedrock().redacted() else {
            panic!("redaction keeps the variant");
        };
        assert_eq!(api_key, Some(ProviderSecret::Redacted), "the key never leaves the process");
        assert_eq!(region, "us-west-2", "everything else survives");
        assert_eq!(profile, Some("voice".to_owned()), "a profile name is not a secret");
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
