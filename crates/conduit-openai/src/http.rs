//! The HTTP plumbing every capability shares.

use conduit_core::{Error, Result};

use crate::failure::Failure;
use crate::OpenAiConfig;

/// A configured client for one server.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    name: String,
}

impl Http {
    /// Builds a client from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the client cannot be built, which happens
    /// when the platform has no usable TLS backend.
    pub fn new(config: &OpenAiConfig) -> Result<Self> {
        let mut builder = reqwest::Client::builder().connect_timeout(config.connect_timeout);
        // A read timeout rather than a total one: synthesis and long answers
        // stream for as long as they need, and capping the whole response would
        // cut off a legitimately long reply. What must be bounded is *silence*.
        if let Some(read_timeout) = config.read_timeout {
            builder = builder.read_timeout(read_timeout);
        }
        let client = builder
            .build()
            .map_err(|error| Error::Config(format!("cannot build an HTTP client: {error}")))?;
        Ok(Self {
            client,
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            name: config.name.clone(),
        })
    }

    /// The provider name, used in errors and registry lookups.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A POST to `path`, authenticated when a key is configured.
    pub fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.authorize(self.client.post(self.endpoint(path)))
    }

    /// A GET from `path`, authenticated when a key is configured.
    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.authorize(self.client.get(self.endpoint(path)))
    }

    /// Wraps a transport failure as a classified provider error.
    #[must_use]
    pub fn failure(&self, error: reqwest::Error) -> Error {
        Error::provider(&self.name, Failure::transport(&error))
    }

    /// Wraps a failure that happened while reading a response body.
    ///
    /// A body can fail two ways that want different answers: the server went
    /// quiet mid-body, which is worth retrying, or it sent something this
    /// provider cannot read, which is not. `reqwest` distinguishes them, so
    /// `what` only has to name the body for the message — "transcription".
    #[must_use]
    pub fn body_failure(&self, what: &str, error: reqwest::Error) -> Error {
        if error.is_timeout() {
            return self.failure(error);
        }
        Error::provider(&self.name, Failure::malformed(format!("unreadable {what}: {error}")))
    }

    /// Sends `request`, turning any non-success status into an error.
    ///
    /// Every error carries a [`Failure`] as its source, so a caller can ask
    /// whether retrying is sensible rather than parsing the message.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] for transport failures and for any status
    /// outside 2xx, naming the status and as much of the body as is likely to
    /// be a message rather than a document.
    pub async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let response = request.send().await.map_err(|error| self.failure(error))?;
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        // The body is the server's own explanation, and it is the difference
        // between "HTTP 400" and "HTTP 400: unknown model `gpt-4o-mimi`". A
        // body that cannot be read does not turn a status failure into a
        // transport one.
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(
            provider = %self.name,
            status = status.as_u16(),
            retry_after = retry_after.as_deref().unwrap_or("-"),
            "provider rejected the request"
        );
        Err(Error::provider(
            &self.name,
            Failure::status_failure(status.as_u16(), retry_after.as_deref(), &body),
        ))
    }

    /// Full URL for a path under the configured base.
    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    /// Applies authentication, when configured.
    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }
}
