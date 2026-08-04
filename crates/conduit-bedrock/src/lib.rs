//! Amazon Bedrock's Converse API as a Conduit language model.
//!
//! A separate crate from [`conduit-anthropic`] rather than a base URL under it,
//! even though the models are largely the same ones. What differs is everything
//! around the model:
//!
//! | | Messages API | Converse |
//! | --- | --- | --- |
//! | Endpoint | one URL | resolved per region by the SDK |
//! | Credential | an API key in a header | SigV4 over a credential chain, or a bearer token |
//! | Transport | server-sent events | an AWS event stream of binary frames |
//! | Wire format | JSON this crate can spell out | generated types the SDK owns |
//! | Model id | a model name | a model, profile, or ARN, prefixed by geography |
//!
//! None of that is reachable with an HTTP client and a base URL: signing a
//! request needs the credential chain, and reading the reply needs an event
//! stream decoder. So this crate is built on `aws-sdk-bedrockruntime` rather
//! than on [`conduit-http`], and what it borrows from Conduit's HTTP layer is
//! the one thing that must not differ — how a failure is classified, so a
//! caller deciding whether to retry gets the same answer it would from any
//! other provider. See [`failure`].
//!
//! ```no_run
//! # async fn example() -> conduit_core::Result<()> {
//! # #[cfg(feature = "bedrock")] {
//! use conduit_bedrock::{Bedrock, BedrockConfig};
//!
//! let claude = Bedrock::new(BedrockConfig {
//!     region: "us-west-2".to_owned(),
//!     ..BedrockConfig::default()
//! })
//! .await?;
//! # }
//! # Ok(())
//! # }
//! ```
//!
//! # Without the `bedrock` feature
//!
//! The AWS SDK is some forty transitive crates, which is a real cost for a
//! deployment that talks to a local model server and nothing else. Compiled
//! without the feature, the provider still exists and still claims its
//! definitions — it refuses to build with a message naming the feature, so an
//! operator learns that this binary cannot reach Bedrock rather than watching a
//! configured model fail its first turn.
//!
//! [`conduit-anthropic`]: https://docs.rs/conduit-anthropic
//! [`conduit-http`]: https://docs.rs/conduit-http

#![cfg_attr(not(feature = "bedrock"), allow(unused_imports))]

use std::time::Duration;

pub mod llm;

#[cfg(feature = "bedrock")]
mod document;
#[cfg(feature = "bedrock")]
mod failure;
#[cfg(feature = "bedrock")]
mod stream;
#[cfg(feature = "bedrock")]
mod wire;

pub use llm::Bedrock;
// Re-exported rather than re-implemented: a caller classifying a failure from
// this provider should not have to know which crate the classification lives
// in, and it is deliberately the same classification either way.
pub use conduit_http::{Failure, FailureKind};

/// Models advertised when a definition names none.
///
/// Inference profile ids rather than bare model ids, and prefixed `us.`: a
/// current Anthropic model on Bedrock is invoked through a cross-region profile,
/// and the bare id is rejected outright. The prefix is a geography, so this list
/// suits a US region; an operator in `eu-west-1` lists `eu.`-prefixed ids of
/// their own, which is what naming models on a definition is for.
pub const DEFAULT_MODELS: &[&str] = &[
    "us.anthropic.claude-opus-4-5-20251101-v1:0",
    "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
    "us.anthropic.claude-haiku-4-5-20251001-v1:0",
];

/// How long to wait to reach the endpoint.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the endpoint may go silent mid-response.
///
/// A read timeout rather than a total one, for the same reason every other
/// provider here draws the line there: a long answer streams for as long as it
/// needs, and thinking can precede the first spoken token by a while. What must
/// be bounded is *silence*.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// How a provider reaches the Bedrock runtime.
///
/// Note what is absent: a base URL. The SDK resolves the endpoint from the
/// region, so a region is the whole of the address, and carrying a URL beside it
/// would be storing a field nothing reads.
#[derive(Debug, Clone)]
pub struct BedrockConfig {
    /// AWS region the model is invoked in, e.g. `us-west-2`.
    pub region: String,
    /// Named profile from the shared AWS config file to load credentials from.
    ///
    /// `None` uses the default chain — environment, task role, instance
    /// profile, default profile — which is what a deployment given its own role
    /// wants.
    pub profile: Option<String>,
    /// A Bedrock API key, sent as a bearer token.
    ///
    /// The long-lived alternative to signing with credentials, for a deployment
    /// with no AWS identity to give this process. `None` signs with whatever the
    /// credential chain resolves.
    pub api_key: Option<String>,
    /// Stable identity, so two differently configured providers can coexist in
    /// one registry.
    ///
    /// This is what the provider calls itself, and what appears in metric
    /// labels and error messages.
    pub name: String,
    /// Human-readable name for operator screens, e.g. `"Claude (Bedrock)"`.
    ///
    /// `None` shows the identity.
    pub label: Option<String>,
    /// How long to wait for the TCP and TLS handshake.
    pub connect_timeout: Duration,
    /// How long the endpoint may go silent before the request is abandoned.
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

impl BedrockConfig {
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

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            // The region every AWS default resolves to when nothing says
            // otherwise, and the one Bedrock documentation writes its examples
            // against.
            region: "us-east-1".to_owned(),
            profile: None,
            api_key: None,
            name: "bedrock".to_owned(),
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
#[cfg_attr(not(feature = "bedrock"), allow(dead_code))]
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
    fn a_configuration_names_a_region_rather_than_a_url() {
        // The endpoint is the SDK's to resolve. A base URL field would be one
        // an operator could fill in and nothing would read.
        let config = BedrockConfig::default();
        assert_eq!(config.region, "us-east-1");
        assert_eq!(config.profile, None, "the default chain, which a task role satisfies");
        assert_eq!(config.api_key, None, "signing rather than a bearer token");
    }

    #[test]
    fn advertised_models_are_inference_profiles_rather_than_bare_model_ids() {
        // A bare `anthropic.claude-...` id is rejected by current models: they
        // are invoked through a cross-region profile, and the profile id is
        // prefixed with the geography it routes within.
        for model in DEFAULT_MODELS {
            assert!(model.starts_with("us."), "`{model}` names no geography");
            assert!(model.contains(":"), "`{model}` names no profile version");
        }
    }

    #[test]
    fn a_request_setting_overrides_a_configured_default_of_the_same_name() {
        let mut defaults = serde_json::Map::new();
        defaults.insert("top_k".to_owned(), serde_json::json!(10));
        defaults.insert("anthropic_beta".to_owned(), serde_json::json!(["x"]));

        let mut request = serde_json::Map::new();
        request.insert("top_k".to_owned(), serde_json::json!(40));

        let merged = layered_settings(&defaults, &request);
        assert_eq!(merged.get("top_k"), Some(&serde_json::json!(40)), "the request wins");
        assert_eq!(
            merged.get("anthropic_beta"),
            Some(&serde_json::json!(["x"])),
            "a default the request does not name carries"
        );
    }
}
