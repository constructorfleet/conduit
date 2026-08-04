//! Anthropic's Messages API as a Conduit language model.
//!
//! A separate crate from [`conduit-openai`] rather than a base URL under it,
//! because the two are different wire formats and not different hosts:
//!
//! | | Chat completions | Messages |
//! | --- | --- | --- |
//! | Credential | `Authorization: Bearer` | `x-api-key` |
//! | Version | in the base URL | `anthropic-version` header |
//! | System prompt | a message with a `system` role | a top-level `system` field |
//! | Response | uniform chunks with choices | typed events over indexed blocks |
//! | Reasoning | a delta field, where offered | its own block type |
//! | `max_tokens` | optional | required |
//! | Sampling | `temperature`, `top_p` | rejected by current models |
//!
//! What the two *do* share — sending an authenticated request, classifying a
//! failure, reassembling server-sent events — lives in [`conduit-http`], which
//! both depend on.
//!
//! ```no_run
//! # use conduit_anthropic::{Anthropic, AnthropicConfig};
//! let claude = Anthropic::new(AnthropicConfig {
//!     api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
//!     ..AnthropicConfig::default()
//! })?;
//! # Ok::<(), conduit_core::Error>(())
//! ```
//!
//! [`conduit-openai`]: https://docs.rs/conduit-openai
//! [`conduit-http`]: https://docs.rs/conduit-http

pub mod llm;
pub mod wire;

mod stream;

use std::time::Duration;

pub use llm::Anthropic;
// Re-exported rather than re-implemented: a caller classifying a failure from
// this provider should not have to know which crate the classification lives
// in, and it is the same classification either way.
pub use conduit_http::{Failure, FailureKind};

/// The public Messages API.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// The API version this crate is written against.
///
/// Pinned rather than tracking whatever is newest: the version header is how
/// the API promises that a response keeps the shape this crate decodes, and
/// following the latest automatically would trade that promise for a surprise.
pub const API_VERSION: &str = "2023-06-01";

/// Models advertised when a definition names none.
///
/// Exact ids, with no date suffix. A definition that lists its own models
/// overrides this; the list exists so a freshly configured provider offers an
/// operator something to choose rather than an empty menu.
pub const DEFAULT_MODELS: &[&str] = &[
    "claude-opus-5",
    "claude-fable-5",
    "claude-sonnet-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
];

/// How long to wait to reach the server.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the server may go silent mid-response.
///
/// A read timeout rather than a total one: a long answer streams for as long as
/// it needs, and thinking can precede the first spoken token by a while. What
/// must be bounded is *silence*.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How a provider reaches the Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// Base URL including any version prefix.
    pub base_url: String,
    /// The API key, sent as `x-api-key`.
    pub api_key: Option<String>,
    /// Stable identity, so two differently configured providers can coexist in
    /// one registry.
    ///
    /// This is what the provider calls itself, and what appears in metric
    /// labels and error messages.
    pub name: String,
    /// Human-readable name for operator screens, e.g. `"Claude (house)"`.
    ///
    /// `None` shows the identity.
    pub label: Option<String>,
    /// How long to wait for the TCP and TLS handshake.
    pub connect_timeout: Duration,
    /// How long the server may go silent before the request is abandoned.
    ///
    /// `None` disables the bound, which is the shape a caller wants only when
    /// something above it already imposes a deadline.
    pub read_timeout: Option<Duration>,
    /// Models this provider advertises. Empty advertises [`DEFAULT_MODELS`].
    pub models: Vec<String>,
    /// A system prompt attached to every turn this provider serves.
    ///
    /// Belongs to the endpoint rather than to any one pipeline. A turn that
    /// carries its own system framing overrides it.
    pub system_prompt: Option<String>,
    /// Default request settings this configured provider applies.
    ///
    /// Checked against the provider's declared schema before they were stored.
    /// They form the base of every request; a setting the request itself
    /// carries overrides the default of the same name.
    pub default_settings: serde_json::Map<String, serde_json::Value>,
}

impl AnthropicConfig {
    /// How to reach this server, as the shared client wants it.
    fn http(&self) -> conduit_http::HttpConfig {
        conduit_http::HttpConfig {
            base_url: self.base_url.clone(),
            name: self.name.clone(),
            // The key travels in a header of its own rather than as a bearer
            // token, and `Credential` is what keeps it out of a log line.
            credential: conduit_http::Credential::header("x-api-key", self.api_key.clone()),
            headers: vec![("anthropic-version".to_owned(), API_VERSION.to_owned())],
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
        }
    }

    /// The identity half of a descriptor.
    fn descriptor(
        &self,
        capability: conduit_provider::Capability,
    ) -> conduit_provider::Descriptor {
        conduit_provider::Descriptor::new(self.name.clone(), capability)
            .with_label(self.label.clone().unwrap_or_else(|| self.name.clone()))
            .with_version(env!("CARGO_PKG_VERSION"))
    }
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_key: None,
            name: "anthropic".to_owned(),
            label: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: Some(DEFAULT_READ_TIMEOUT),
            models: Vec::new(),
            system_prompt: None,
            default_settings: serde_json::Map::new(),
        }
    }
}

/// A request's settings layered over the provider's configured defaults.
///
/// The Configured Provider's stored settings are the base; a setting the request
/// carries of the same name wins, so a pipeline can still override what the
/// operator set as a default. Both were checked against the same schema, so the
/// result is too.
pub(crate) fn layered_settings(
    defaults: &serde_json::Map<String, serde_json::Value>,
    request: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = defaults.clone();
    for (name, value) in request {
        merged.insert(name.clone(), value.clone());
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_header_is_pinned_on_every_request() {
        let headers = AnthropicConfig::default().http().headers;
        assert_eq!(headers, [("anthropic-version".to_owned(), API_VERSION.to_owned())]);
    }

    #[test]
    fn the_key_travels_as_a_header_rather_than_a_bearer_token() {
        let config =
            AnthropicConfig { api_key: Some("sk-ant-test".to_owned()), ..Default::default() };

        assert_eq!(
            config.http().credential,
            conduit_http::Credential::Header {
                name: "x-api-key".to_owned(),
                value: "sk-ant-test".to_owned(),
            }
        );
    }

    #[test]
    fn no_key_is_no_credential_rather_than_an_empty_header() {
        assert!(!AnthropicConfig::default().http().credential.is_some());
    }

    #[test]
    fn advertised_models_carry_no_date_suffix() {
        // A dated id is a legacy Bedrock spelling and is rejected here.
        for model in DEFAULT_MODELS {
            assert!(
                !model.contains("-2024") && !model.contains("-2025"),
                "`{model}` looks like a dated id"
            );
        }
    }
}
