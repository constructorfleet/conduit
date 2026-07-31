//! Language models over the chat completions API.

use conduit_core::Result;
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel};
use conduit_provider::{ChunkStream, Health, Provider};

use crate::http::Http;
use crate::{stream, OpenAiConfig};

/// A language model served over the chat completions API.
#[derive(Debug, Clone)]
pub struct OpenAi {
    http: Http,
    models: Vec<String>,
}

impl OpenAi {
    /// Builds a provider from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built, which
    /// happens when the platform has no usable TLS backend.
    pub fn new(config: OpenAiConfig) -> Result<Self> {
        Ok(Self { http: Http::new(&config)?, models: config.models })
    }
}

#[async_trait::async_trait]
impl Provider for OpenAi {
    fn name(&self) -> &str {
        self.http.name()
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
        let body = crate::wire::Request::from_completion(request);
        tracing::debug!(model = %body.model, tools = body.tools.len(), "requesting completion");

        let response = self.http.send(self.http.post("chat/completions").json(&body)).await?;
        Ok(stream::completions(response, self.http.name().to_owned()))
    }

    fn models(&self) -> &[String] {
        &self.models
    }

    fn supports_tools(&self) -> bool {
        true
    }
}
