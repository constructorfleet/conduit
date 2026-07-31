//! The HTTP plumbing every capability shares.

use conduit_core::{Error, Result};

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
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
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

    /// Wraps a transport failure as a provider error.
    #[must_use]
    pub fn failure(&self, error: reqwest::Error) -> Error {
        Error::provider(&self.name, error)
    }

    /// Sends `request`, turning any non-success status into an error.
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
        let body = response.text().await.unwrap_or_default();
        let detail = body.chars().take(512).collect::<String>();
        Err(Error::provider(
            &self.name,
            std::io::Error::other(format!("HTTP {status}: {detail}")),
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
