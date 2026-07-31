//! An OpenAI-compatible language model provider.
//!
//! The chat completions API is the closest thing to a lingua franca among
//! model servers, so one implementation covers OpenAI, Ollama, vLLM, LM
//! Studio, OpenRouter, and anything else that speaks it. Only the base URL
//! changes.
//!
//! ```no_run
//! # use conduit_openai::{OpenAi, OpenAiConfig};
//! // A local Ollama server.
//! let local = OpenAi::new(OpenAiConfig {
//!     base_url: "http://localhost:11434/v1".to_owned(),
//!     ..OpenAiConfig::default()
//! })?;
//!
//! // Or the hosted API.
//! let hosted = OpenAi::new(OpenAiConfig {
//!     api_key: std::env::var("OPENAI_API_KEY").ok(),
//!     ..OpenAiConfig::default()
//! })?;
//! # Ok::<(), conduit_core::Error>(())
//! ```

pub mod sse;
pub mod wire;

mod stream;

use std::time::Duration;

use conduit_core::{Error, Result};
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel};
use conduit_provider::{ChunkStream, Health, Provider};

/// The public OpenAI API.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// How a provider reaches its server.
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// Base URL including any version prefix, e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    /// Bearer token. Local servers usually need none.
    pub api_key: Option<String>,
    /// Registration name, so two differently configured servers can coexist
    /// in one registry — `"openai"` and `"ollama"`, say.
    pub name: String,
    /// How long to wait for the response head. The body then streams for as
    /// long as it needs, so this does not cap a long answer.
    pub connect_timeout: Duration,
    /// Models this provider advertises. Empty passes any name through.
    pub models: Vec<String>,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: None,
            name: "openai".to_owned(),
            connect_timeout: Duration::from_secs(30),
            models: Vec::new(),
        }
    }
}

/// A language model served over the chat completions API.
#[derive(Debug, Clone)]
pub struct OpenAi {
    config: OpenAiConfig,
    client: reqwest::Client,
}

impl OpenAi {
    /// Builds a provider from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built, which
    /// happens when the platform has no usable TLS backend.
    pub fn new(config: OpenAiConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .build()
            .map_err(|error| Error::Config(format!("cannot build an HTTP client: {error}")))?;
        Ok(Self { config, client })
    }

    /// Full URL for a path under the configured base.
    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Applies authentication, when configured.
    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.config.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }

    /// Turns a non-success response into an error naming the status.
    ///
    /// The body is included when it is short enough to be a message rather
    /// than a document; it usually explains the refusal.
    async fn rejection(&self, response: reqwest::Response) -> Error {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let detail = body.chars().take(512).collect::<String>();
        Error::provider(
            &self.config.name,
            std::io::Error::other(format!("HTTP {status}: {detail}")),
        )
    }
}

#[async_trait::async_trait]
impl Provider for OpenAi {
    fn name(&self) -> &str {
        &self.config.name
    }

    async fn health(&self) -> Health {
        let request = self.authorize(self.client.get(self.endpoint("models")));
        match request.send().await {
            Ok(response) if response.status().is_success() => Health::Healthy,
            Ok(response) => Health::Unhealthy {
                reason: format!("model listing returned HTTP {}", response.status()),
            },
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl LanguageModel for OpenAi {
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream<Completion>> {
        let body = wire::Request::from_completion(request);
        tracing::debug!(model = %body.model, tools = body.tools.len(), "requesting completion");

        let response = self
            .authorize(self.client.post(self.endpoint("chat/completions")))
            .json(&body)
            .send()
            .await
            .map_err(|error| Error::provider(&self.config.name, error))?;

        if !response.status().is_success() {
            return Err(self.rejection(response).await);
        }

        Ok(stream::completions(response, self.config.name.clone()))
    }

    fn models(&self) -> &[String] {
        &self.config.models
    }

    fn supports_tools(&self) -> bool {
        true
    }
}
