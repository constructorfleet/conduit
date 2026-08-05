//! Embeddings over the OpenAI-compatible `/embeddings` endpoint.
//!
//! A plain struct rather than a [`Provider`], deliberately. There is no
//! embedding capability in Conduit and there should not be one: a capability is
//! something an operator binds to a pipeline node, and nobody binds "turn text
//! into a vector" to a node — it is a dependency of whatever needed the vector,
//! which today means a vector memory store. Adding
//! `ProviderCapability::Embedding` would mean touching the capability enum,
//! every mapping function over it, the registry, and the operator console, to
//! surface something no graph can use.
//!
//! ```no_run
//! # use conduit_openai::{OpenAiConfig, OpenAiEmbeddings};
//! # async fn example() -> conduit_core::Result<()> {
//! let embeddings = OpenAiEmbeddings::new(&OpenAiConfig::default(), "text-embedding-3-small")?;
//! let vector = embeddings.embed("the recycling goes out on tuesday").await?;
//! # Ok(())
//! # }
//! ```
//!
//! [`Provider`]: conduit_provider::Provider

use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::OpenAiConfig;
use conduit_http::Failure;
use conduit_http::Http;

/// An embedding request in the vendor's shape.
///
/// `input` is a list even for one text, because that is the field's type and a
/// server is entitled to reject a bare string.
#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    input: [&'a str; 1],
}

/// The vendor's reply: a list of embeddings, in request order.
#[derive(Debug, Deserialize)]
struct Response {
    data: Vec<Embedding>,
}

/// One embedding from the reply.
#[derive(Debug, Deserialize)]
struct Embedding {
    embedding: Vec<f32>,
}

/// Turns text into a vector using an OpenAI-compatible embeddings endpoint.
///
/// Served by the hosted API and by Ollama, vLLM, LM Studio, and
/// `text-embeddings-inference`, so one implementation reaches all of them.
#[derive(Debug, Clone)]
pub struct OpenAiEmbeddings {
    http: Http,
    model: String,
}

impl OpenAiEmbeddings {
    /// Builds an embedder using `model`, e.g. `"text-embedding-3-small"`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built.
    ///
    /// [`Error::Config`]: conduit_core::Error::Config
    pub fn new(config: &OpenAiConfig, model: impl Into<String>) -> Result<Self> {
        Ok(Self { http: Http::new(config.http())?, model: model.into() })
    }

    /// The model this embedder asks for.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The name this embedder reports in errors, from its configuration.
    #[must_use]
    pub fn name(&self) -> &str {
        self.http.name()
    }

    /// Embeds one text.
    ///
    /// One text per call rather than a batch: the caller embeds a question
    /// before a turn and a record after one, always one at a time, and a batch
    /// API nobody calls in batches is an interface with an untested path in it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if the server cannot be reached, rejects the
    /// request, or answers with no embedding in it. An empty reply is a failure
    /// rather than an empty vector: a zero-length vector would be stored and
    /// then match nothing forever, silently.
    ///
    /// [`Error::Provider`]: conduit_core::Error::Provider
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let body = Request { model: &self.model, input: [text] };
        // The text itself is never logged: it is what somebody said to the
        // assistant. Its length is enough to diagnose a rejected request.
        tracing::debug!(model = %self.model, characters = text.len(), "requesting an embedding");

        let response = self.http.send(self.http.post("embeddings").json(&body)).await?;
        let decoded: Response = response
            .json()
            .await
            .map_err(|error| self.http.body_failure("embedding", error))?;

        decoded.data.into_iter().next().map(|first| first.embedding).ok_or_else(|| {
            conduit_core::Error::provider(
                self.http.name(),
                Failure::malformed("the embeddings endpoint returned no embedding"),
            )
        })
    }
}
