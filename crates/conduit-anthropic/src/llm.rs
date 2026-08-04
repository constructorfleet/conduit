//! Language models over the Messages API.

use conduit_core::Result;
use conduit_http::Http;
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};

use crate::{stream, AnthropicConfig, DEFAULT_MODELS};

/// The request controls the Messages API accepts beyond the ones every model
/// has.
///
/// Declared rather than passed through, so a caller who misspells one is told
/// rather than silently ignored. Sampling controls are conspicuously absent:
/// `temperature`, `top_p` and `top_k` are rejected outright by current models,
/// so declaring them would invite an operator to configure a 400.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "output_config": {
                "type": "object",
                "description":
                    "Response controls. `effort` trades thoroughness against latency and \
                     cost: one of `low`, `medium`, `high`, `xhigh`, `max`.",
            },
            "thinking": {
                "type": "object",
                "description":
                    "Extended thinking. `{\"type\":\"adaptive\"}` lets the model decide how \
                     much to think; add `\"display\":\"summarized\"` to receive the reasoning, \
                     which current models omit by default. Reasoning is never spoken aloud.",
            },
            "stop_sequences": {
                "type": "array",
                "description": "Text that ends the response when the model produces it.",
            },
            "metadata": {
                "type": "object",
                "description": "Opaque request metadata, e.g. a `user_id` for abuse tracking.",
            },
        },
    }))
    .expect("a literal object schema")
}

/// A language model served over the Messages API.
#[derive(Debug, Clone)]
pub struct Anthropic {
    http: Http,
    descriptor: Descriptor,
    system_prompt: Option<String>,
    default_settings: serde_json::Map<String, serde_json::Value>,
}

impl Anthropic {
    /// Builds a provider from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`](conduit_core::Error::Config) if the HTTP
    /// client cannot be built, which happens when the platform has no usable
    /// TLS backend.
    pub fn new(config: AnthropicConfig) -> Result<Self> {
        // An empty list is not an empty catalogue: it means the definition
        // named no models, so the current ones are advertised rather than
        // leaving an operator with nothing to choose from.
        let models = if config.models.is_empty() {
            DEFAULT_MODELS.iter().map(|model| (*model).to_owned()).collect()
        } else {
            config.models.clone()
        };

        let descriptor = config
            .descriptor(Capability::Llm)
            .with_metadata(Metadata::default().with_models(models).with_tools())
            .with_settings(settings_schema());

        Ok(Self {
            http: Http::new(config.http())?,
            descriptor,
            system_prompt: config.system_prompt,
            default_settings: config.default_settings,
        })
    }
}

#[async_trait::async_trait]
impl Provider for Anthropic {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        // The Messages API has no unauthenticated liveness route, and listing
        // models is the cheapest call that exercises the credential as well as
        // the connection — an unreachable server and a rejected key are both
        // things an operator needs to see before a turn discovers them.
        match self.http.send(self.http.get("models")).await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl LanguageModel for Anthropic {
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream<Completion>> {
        let body = crate::wire::Request::from_completion(
            request,
            &self.default_settings,
            self.system_prompt.as_deref(),
        );
        tracing::debug!(
            model = %body.model,
            tools = body.tools.len(),
            max_tokens = body.max_tokens,
            "requesting completion"
        );

        let response = self.http.send(self.http.post("messages").json(&body)).await?;
        Ok(stream::completions(response, self.http.name().to_owned()))
    }

    fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> Anthropic {
        Anthropic::new(AnthropicConfig {
            name: "claude".to_owned(),
            label: Some("Claude (house)".to_owned()),
            models: vec!["claude-opus-5".to_owned()],
            ..AnthropicConfig::default()
        })
        .expect("client")
    }

    #[test]
    fn one_descriptor_answers_what_the_console_asks() {
        let descriptor = provider().descriptor().clone();

        assert_eq!(descriptor.id, "claude");
        assert_eq!(descriptor.label, "Claude (house)");
        assert_eq!(descriptor.capability, Capability::Llm);
        assert_eq!(descriptor.metadata.models, ["claude-opus-5"]);
        assert!(descriptor.metadata.tools, "the Messages API calls tools");
        assert!(!descriptor.settings.is_empty(), "the request controls are declared");
    }

    #[test]
    fn naming_no_models_advertises_the_current_ones() {
        // An operator who has not typed a model list should still be offered
        // something to pick, rather than an empty menu.
        let provider = Anthropic::new(AnthropicConfig::default()).expect("client");
        let models = &provider.descriptor().metadata.models;

        assert!(models.contains(&"claude-opus-5".to_owned()), "{models:?}");
        assert_eq!(models.len(), DEFAULT_MODELS.len());
    }

    #[test]
    fn a_label_falls_back_to_the_identity() {
        let provider = Anthropic::new(AnthropicConfig::default()).expect("client");
        assert_eq!(provider.descriptor().label, "anthropic");
    }

    #[test]
    fn a_provider_never_prints_its_key() {
        // Providers derive `Debug`, and this one holds a credential.
        let provider = Anthropic::new(AnthropicConfig {
            api_key: Some("sk-ant-secret".to_owned()),
            ..AnthropicConfig::default()
        })
        .expect("client");

        let printed = format!("{provider:?}");
        assert!(!printed.contains("sk-ant-secret"), "{printed}");
    }

    #[test]
    fn sampling_controls_are_refused_rather_than_sent_as_a_400() {
        // Current models reject `temperature`, `top_p` and `top_k`. Refusing
        // them at the schema tells an operator when they save the definition,
        // rather than when a conversation fails.
        let provider = provider();
        for rejected in ["temperature", "top_p", "top_k"] {
            assert!(
                provider
                    .descriptor()
                    .validate_settings(&serde_json::json!({ rejected: 0.5 }))
                    .is_err(),
                "`{rejected}` is not a setting this API accepts"
            );
        }
    }

    #[test]
    fn declared_settings_reach_the_wire() {
        let provider = provider();
        let settings = provider
            .descriptor()
            .validate_settings(&serde_json::json!({
                "thinking": { "type": "adaptive", "display": "summarized" },
            }))
            .expect("a declared setting");

        let body = crate::wire::Request::from_completion(
            CompletionRequest {
                settings,
                ..CompletionRequest::new("claude-opus-5", Vec::new())
            },
            &serde_json::Map::new(),
            None,
        );

        assert_eq!(
            body.settings.get("thinking"),
            Some(&serde_json::json!({ "type": "adaptive", "display": "summarized" }))
        );
    }
}
