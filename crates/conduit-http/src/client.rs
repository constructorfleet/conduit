//! The HTTP plumbing every HTTP-backed provider shares.

use std::time::Duration;

use conduit_core::{Error, Result};

use crate::failure::Failure;

/// A credential that has to be fetched afresh for each request.
///
/// Implemented by a provider whose credential expires: a Google access token
/// from Application Default Credentials lives about an hour and the library
/// underneath refreshes it, so a client that captured one string would keep
/// sending it long after it stopped working.
///
/// The returned token is used as `Authorization: Bearer <token>`, which is the
/// scheme every short-lived credential in this workspace uses.
#[async_trait::async_trait]
pub trait BearerSource: std::fmt::Debug + Send + Sync {
    /// A token good for the next request.
    ///
    /// # Errors
    ///
    /// Returns an error if the credential cannot be obtained or refreshed — an
    /// expired refresh token, a metadata server that has gone away, a revoked
    /// service account.
    async fn token(&self) -> Result<std::sync::Arc<str>>;
}

/// How a provider proves who it is.
///
/// Named rather than passed as an optional token because vendors disagree on
/// the mechanism, not just the value: OpenAI-compatible servers take a bearer
/// token, Anthropic takes the key in `x-api-key`, and a local server takes
/// nothing at all. A caller that only had `Option<String>` had to hope the
/// scheme its vendor wanted was the one baked in here.
#[derive(Clone)]
pub enum Credential {
    /// No credential, which is the usual shape for a server on the LAN.
    None,
    /// `Authorization: Bearer <token>`.
    Bearer(String),
    /// The key in a vendor-specific header, e.g. `x-api-key`.
    Header {
        /// Header name.
        name: String,
        /// Header value, which is the secret itself.
        value: String,
    },
    /// A bearer token fetched per request, for a credential that expires.
    ///
    /// Unlike the other variants this one can *fail* at send time, because
    /// obtaining it is a fallible operation rather than a lookup.
    Refreshed(std::sync::Arc<dyn BearerSource>),
}

impl Credential {
    /// A bearer token, or [`Self::None`] when there is no token.
    #[must_use]
    pub fn bearer(token: Option<String>) -> Self {
        token.map_or(Self::None, Self::Bearer)
    }

    /// A key carried in `name`, or [`Self::None`] when there is no key.
    #[must_use]
    pub fn header(name: impl Into<String>, key: Option<String>) -> Self {
        key.map_or(Self::None, |value| Self::Header { name: name.into(), value })
    }

    /// A bearer token fetched afresh for every request.
    #[must_use]
    pub fn refreshed(source: impl BearerSource + 'static) -> Self {
        Self::Refreshed(std::sync::Arc::new(source))
    }

    /// Whether a credential was configured at all.
    #[must_use]
    pub const fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Applies this credential to `request`.
    ///
    /// Async because [`Self::Refreshed`] has to go and get a token, which can
    /// fail. That is also why this happens when the request is sent rather than
    /// when it is built: a builder cannot await, and a token fetched any earlier
    /// than the send is a token that might already have expired.
    async fn apply(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        Ok(match self {
            Self::None => request,
            // `bearer_auth` marks the header sensitive, so a `reqwest` trace of
            // the request elides it rather than printing it.
            Self::Bearer(token) => request.bearer_auth(token),
            Self::Header { name, value } => request.header(name, value),
            Self::Refreshed(source) => request.bearer_auth(&*source.token().await?),
        })
    }
}

/// Compares what a credential *is*, never what it holds.
///
/// Two refreshed credentials are the same when they draw on the same source,
/// which is the only comparison available: a token source has no value to
/// compare, and asking it for one to find out would be a network call.
impl PartialEq for Credential {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::None, Self::None) => true,
            (Self::Bearer(ours), Self::Bearer(theirs)) => ours == theirs,
            (
                Self::Header { name: ours, value: our_value },
                Self::Header { name: theirs, value: their_value },
            ) => ours == theirs && our_value == their_value,
            (Self::Refreshed(ours), Self::Refreshed(theirs)) => std::ptr::eq(
                std::sync::Arc::as_ptr(ours).cast::<u8>(),
                std::sync::Arc::as_ptr(theirs).cast::<u8>(),
            ),
            _ => false,
        }
    }
}

impl Eq for Credential {}

/// Prints whether a credential exists, never what it is.
///
/// A provider holding one of these derives `Debug`, so anything this type
/// prints can reach a log line — and a logged API key is a leaked API key.
impl std::fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => formatter.write_str("Credential::None"),
            Self::Bearer(_) => formatter.write_str("Credential::Bearer(<redacted>)"),
            Self::Header { name, .. } => {
                write!(formatter, "Credential::Header {{ name: {name:?}, value: <redacted> }}")
            }
            Self::Refreshed(_) => formatter.write_str("Credential::Refreshed(<redacted>)"),
        }
    }
}

/// How to reach one server.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Base URL including any version prefix.
    pub base_url: String,
    /// The provider identity errors and metrics are labelled with.
    pub name: String,
    /// How the request authenticates.
    pub credential: Credential,
    /// Headers sent on every request, for vendors that pin an API version or
    /// opt into a feature that way. Never a place for a secret: use
    /// [`Credential::Header`], which does not print itself.
    pub headers: Vec<(String, String)>,
    /// How long to wait for the TCP and TLS handshake.
    pub connect_timeout: Duration,
    /// How long the server may go silent before the request is abandoned.
    pub read_timeout: Option<Duration>,
}

/// A configured client for one server.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
    base_url: String,
    credential: Credential,
    headers: Vec<(String, String)>,
    name: String,
}

impl Http {
    /// Builds a client from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the client cannot be built, which happens
    /// when the platform has no usable TLS backend.
    pub fn new(config: HttpConfig) -> Result<Self> {
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
            base_url: config.base_url,
            credential: config.credential,
            headers: config.headers,
            name: config.name,
        })
    }

    /// The provider name, used in errors and registry lookups.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A POST to `path`, carrying any pinned headers.
    ///
    /// The credential is attached by [`Self::send`], not here — see
    /// [`Credential::apply`].
    pub fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.prepare(self.client.post(self.endpoint(path)))
    }

    /// A GET from `path`, carrying any pinned headers.
    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.prepare(self.client.get(self.endpoint(path)))
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

    /// Wraps a response this provider could not interpret.
    #[must_use]
    pub fn malformed(&self, detail: impl Into<String>) -> Error {
        Error::provider(&self.name, Failure::malformed(detail))
    }

    /// Sends `request`, turning any non-success status into an error.
    ///
    /// Every error carries a [`Failure`] as its source, so a caller can ask
    /// whether retrying is sensible rather than parsing the message.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] for transport failures, for any status
    /// outside 2xx — naming the status and as much of the body as is likely to
    /// be a message rather than a document — and for a
    /// [`Credential::Refreshed`] that could not be obtained.
    pub async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let request = self.credential.apply(request).await?;
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

    /// Applies the headers pinned for every request.
    fn prepare(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        self.headers.iter().fold(request, |request, (name, value)| request.header(name, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> HttpConfig {
        HttpConfig {
            base_url: "https://api.example/v1".to_owned(),
            name: "example".to_owned(),
            credential: Credential::None,
            headers: Vec::new(),
            connect_timeout: Duration::from_secs(1),
            read_timeout: None,
        }
    }

    #[test]
    fn a_credential_never_prints_itself() {
        // A provider holding one of these derives `Debug`, so this is the only
        // thing standing between an API key and a log line.
        let bearer = format!("{:?}", Credential::Bearer("sk-secret".to_owned()));
        assert!(!bearer.contains("sk-secret"), "{bearer}");

        let keyed = format!(
            "{:?}",
            Credential::Header {
                name: "x-api-key".to_owned(),
                value: "sk-ant-secret".to_owned(),
            }
        );
        assert!(!keyed.contains("sk-ant-secret"), "{keyed}");
        assert!(keyed.contains("x-api-key"), "the header name is not the secret: {keyed}");
    }

    #[test]
    fn a_client_holding_a_credential_never_prints_it_either() {
        let http = Http::new(HttpConfig {
            credential: Credential::Header {
                name: "x-api-key".to_owned(),
                value: "sk-ant-secret".to_owned(),
            },
            ..config()
        })
        .expect("client");

        let printed = format!("{http:?}");
        assert!(!printed.contains("sk-ant-secret"), "{printed}");
    }

    #[test]
    fn a_path_joins_the_base_without_doubling_the_slash() {
        let http = Http::new(HttpConfig {
            base_url: "https://api.example/v1/".to_owned(),
            ..config()
        })
        .expect("client");

        assert_eq!(http.endpoint("/messages"), "https://api.example/v1/messages");
        assert_eq!(http.endpoint("messages"), "https://api.example/v1/messages");
    }

    /// A source that hands out a different token every time it is asked.
    #[derive(Debug, Default)]
    struct Counting(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl BearerSource for Counting {
        async fn token(&self) -> Result<std::sync::Arc<str>> {
            let nth = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(std::sync::Arc::from(format!("token-{nth}").as_str()))
        }
    }

    #[tokio::test]
    async fn a_refreshed_credential_is_fetched_for_every_request() {
        // The whole reason this variant exists: a token captured once keeps
        // being sent for an hour after it expires. Two applications of the same
        // credential must ask twice.
        let credential = Credential::refreshed(Counting::default());
        let client = reqwest::Client::new();

        for expected in ["token-0", "token-1"] {
            let request = credential
                .apply(client.get("https://api.example/v1/models"))
                .await
                .expect("a token");
            let built = request.build().expect("a request");
            let header = built
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .expect("an authorization header");
            assert_eq!(header.to_str().expect("ascii"), format!("Bearer {expected}"));
            assert!(header.is_sensitive(), "a token must not reach a request trace");
        }
    }

    #[derive(Debug)]
    struct Refusing;

    #[async_trait::async_trait]
    impl BearerSource for Refusing {
        async fn token(&self) -> Result<std::sync::Arc<str>> {
            Err(Error::Config("no credentials on this host".to_owned()))
        }
    }

    #[tokio::test]
    async fn a_credential_that_cannot_be_obtained_fails_the_request_rather_than_sending_it() {
        // Sending it unauthenticated would earn a 401 that reads like a
        // permissions problem, hiding the real cause.
        let error = Credential::refreshed(Refusing)
            .apply(reqwest::Client::new().get("https://api.example/v1/models"))
            .await
            .expect_err("no token, no request");
        assert!(error.to_string().contains("no credentials"), "{error}");
    }

    #[test]
    fn a_refreshed_credential_never_prints_its_source() {
        let printed = format!("{:?}", Credential::refreshed(Counting::default()));
        assert!(printed.contains("<redacted>"), "{printed}");
    }

    #[test]
    fn a_missing_token_is_no_credential_rather_than_an_empty_one() {
        assert_eq!(Credential::bearer(None), Credential::None);
        assert!(!Credential::bearer(None).is_some());
        assert_eq!(
            Credential::bearer(Some("t".to_owned())),
            Credential::Bearer("t".to_owned())
        );
        assert_eq!(Credential::header("x-api-key", None), Credential::None);
    }
}
