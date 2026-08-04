//! The HTTP plumbing both capabilities share.
//!
//! Two Google services, two hostnames, one shape of request: a JSON body posted
//! to a `resource:verb` path with a bearer token. The token is fetched per
//! request rather than held, because [`Tokens`] refreshes underneath and a
//! provider that cached one would keep using it for an hour after it expired.

use conduit_core::{Error, Result};

use crate::auth::Tokens;
use crate::failure::Failure;

/// A configured client for one Google service.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
    base_url: String,
    tokens: Tokens,
    name: String,
}

impl Http {
    /// Builds a client for the service at `base_url`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the client cannot be built, which happens
    /// when the platform has no usable TLS backend.
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        tokens: Tokens,
        connect_timeout: std::time::Duration,
        read_timeout: Option<std::time::Duration>,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder().connect_timeout(connect_timeout);
        // A read timeout rather than a total one: what must be bounded is
        // *silence*, and a long recording legitimately takes a long time to
        // recognize.
        if let Some(read_timeout) = read_timeout {
            builder = builder.read_timeout(read_timeout);
        }
        let client = builder
            .build()
            .map_err(|error| Error::Config(format!("cannot build an HTTP client: {error}")))?;
        Ok(Self { client, base_url: base_url.into(), tokens, name: name.into() })
    }

    /// The provider name, used in errors and registry lookups.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// An authenticated POST of `body` to `path`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if a token cannot be obtained, if the request
    /// fails at the transport layer, or if the service answers outside 2xx.
    pub async fn post_json<B: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<reqwest::Response> {
        let request = self.client.post(self.endpoint(path)).json(body);
        self.send(request).await
    }

    /// An authenticated GET from `path`, with `query` appended as parameters.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if a token cannot be obtained, if the request
    /// fails at the transport layer, or if the service answers outside 2xx.
    pub async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<reqwest::Response> {
        // `reqwest` percent-encodes query values, so a language tag with a
        // surprise in it cannot escape its parameter and become another one.
        let request = self.client.get(self.endpoint(path)).query(query);
        self.send(request).await
    }

    /// Wraps a transport failure as a classified provider error.
    #[must_use]
    pub fn failure(&self, error: reqwest::Error) -> Error {
        Error::provider(&self.name, Failure::transport(&error))
    }

    /// Wraps a failure that happened while reading a response body.
    ///
    /// A body can fail two ways that want different answers: the service went
    /// quiet mid-body, which is worth retrying, or it sent something this
    /// provider cannot read, which is not. `reqwest` distinguishes them, so
    /// `what` only has to name the body for the message.
    #[must_use]
    pub fn body_failure(&self, what: &str, error: reqwest::Error) -> Error {
        if error.is_timeout() {
            return self.failure(error);
        }
        Error::provider(&self.name, Failure::malformed(format!("unreadable {what}: {error}")))
    }

    /// A malformed-response error naming `detail`.
    #[must_use]
    pub fn malformed(&self, detail: impl Into<String>) -> Error {
        Error::provider(&self.name, Failure::malformed(detail))
    }

    /// Authenticates and sends `request`, turning any non-success status into an
    /// error.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let token = self.tokens.bearer(&self.name).await?;
        // `bearer_auth` marks the header sensitive, so a `reqwest` trace of the
        // request elides it rather than printing it.
        let response =
            request.bearer_auth(&*token).send().await.map_err(|error| self.failure(error))?;
        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        // The body is Google's own explanation, and it is the difference between
        // "HTTP 400" and "HTTP 400: Invalid recognition config". A body that
        // cannot be read does not turn a status failure into a transport one.
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(
            provider = %self.name,
            status = status.as_u16(),
            retry_after = retry_after.as_deref().unwrap_or("-"),
            "Google rejected the request"
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn client(base_url: &str) -> Http {
        Http::new(
            "google",
            base_url,
            Tokens::Fixed(Arc::from("t0ken")),
            std::time::Duration::from_secs(1),
            None,
        )
        .expect("a client")
    }

    #[test]
    fn a_path_joins_the_base_with_exactly_one_slash() {
        let joined = client("https://texttospeech.googleapis.com/v1");
        assert_eq!(
            joined.endpoint("text:synthesize"),
            "https://texttospeech.googleapis.com/v1/text:synthesize"
        );

        let trailing = client("https://texttospeech.googleapis.com/v1/");
        assert_eq!(
            trailing.endpoint("/text:synthesize"),
            "https://texttospeech.googleapis.com/v1/text:synthesize"
        );
    }

    #[test]
    fn a_client_never_renders_its_token() {
        assert!(!format!("{:?}", client("https://example.test")).contains("t0ken"));
    }
}
