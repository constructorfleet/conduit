//! Language models over the chat completions API.

use conduit_core::Result;
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};

use crate::{stream, OpenAiConfig};
use conduit_http::Http;

/// The sampling controls the chat completions API accepts beyond the ones
/// every model has.
///
/// Declared rather than passed through: a server that has never heard of
/// `seed` will ignore it, but a caller who wrote `top-p` instead of `top_p`
/// used to get silence, and now gets told.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "top_p": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0,
                "description": "Nucleus sampling cutoff.",
            },
            "frequency_penalty": {
                "type": "number",
                "minimum": -2.0,
                "maximum": 2.0,
                "description": "Discourages repeating tokens already used.",
            },
            "presence_penalty": {
                "type": "number",
                "minimum": -2.0,
                "maximum": 2.0,
                "description": "Discourages repeating topics already raised.",
            },
            "seed": {
                "type": "integer",
                "description": "Requests reproducible sampling, where the server supports it.",
            },
        },
    }))
    .expect("a literal object schema")
}

/// A language model served over the chat completions API.
#[derive(Debug, Clone)]
pub struct OpenAi {
    http: Http,
    descriptor: Descriptor,
    system_prompt: Option<String>,
    default_settings: serde_json::Map<String, serde_json::Value>,
}

impl OpenAi {
    /// Builds a provider from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built, which
    /// happens when the platform has no usable TLS backend.
    pub fn new(config: OpenAiConfig) -> Result<Self> {
        let descriptor = config
            .descriptor(Capability::Llm)
            .with_metadata(Metadata::default().with_models(config.models.clone()).with_tools())
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
impl Provider for OpenAi {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        match self.http.send(self.http.get("models")).await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl LanguageModel for OpenAi {
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream<Completion>> {
        let body = crate::wire::Request::from_completion(request, &self.default_settings);
        tracing::debug!(model = %body.model, tools = body.tools.len(), "requesting completion");

        let response = self.http.send(self.http.post("chat/completions").json(&body)).await?;
        Ok(stream::completions(response, self.http.name().to_owned()))
    }

    fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> OpenAi {
        OpenAi::new(OpenAiConfig {
            name: "ollama".to_owned(),
            label: Some("Ollama (kitchen)".to_owned()),
            models: vec!["llama3.1:8b".to_owned()],
            ..OpenAiConfig::default()
        })
        .expect("client")
    }

    #[test]
    fn one_descriptor_answers_what_used_to_be_four_methods() {
        let descriptor = provider().descriptor().clone();

        assert_eq!(descriptor.id, "ollama");
        assert_eq!(descriptor.label, "Ollama (kitchen)");
        assert_eq!(descriptor.capability, Capability::Llm);
        assert_eq!(descriptor.metadata.models, ["llama3.1:8b"]);
        assert!(descriptor.metadata.tools, "chat completions calls tools");
        assert!(!descriptor.settings.is_empty(), "the sampling controls are declared");
    }

    #[test]
    fn a_label_falls_back_to_the_identity() {
        let provider = OpenAi::new(OpenAiConfig::default()).expect("client");
        assert_eq!(provider.descriptor().label, "openai");
    }

    #[test]
    fn declared_settings_reach_the_wire_and_undeclared_ones_are_refused() {
        let provider = provider();
        let settings = provider
            .descriptor()
            .validate_settings(&serde_json::json!({ "top_p": 0.2 }))
            .expect("a declared setting");
        let request = crate::wire::Request::from_completion(
            conduit_provider::llm::CompletionRequest {
                settings,
                ..conduit_provider::llm::CompletionRequest::new("llama3.1:8b", Vec::new())
            },
            &serde_json::Map::new(),
        );
        assert_eq!(request.settings.get("top_p"), Some(&serde_json::json!(0.2)));

        assert!(
            provider
                .descriptor()
                .validate_settings(&serde_json::json!({ "top-p": 0.2 }))
                .is_err(),
            "a setting the API does not have is refused rather than sent"
        );
    }
}
