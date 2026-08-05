//! Language models over the Converse API.

use conduit_core::Result;
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};

use crate::BedrockConfig;

/// The request controls Converse accepts beyond the ones every model has.
///
/// Declared rather than passed through, so a caller who misspells one is told
/// rather than silently ignored. What is declared here is narrower than the
/// Messages API's list and wider in one place: `temperature` and `top_p` belong
/// to Converse's own inference configuration and are set from the request, while
/// anything model-specific — `top_k`, `thinking`, `anthropic_beta` — travels in
/// `additionalModelRequestFields`, which is the field this schema describes.
/// Which of those a given model accepts depends on the model, and Bedrock is a
/// door onto many, so they are declared as the open object the API treats them
/// as.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "top_k": {
                "type": "integer",
                "description":
                    "Sample from the k most likely tokens. Model-specific: sent as an \
                     additional model request field, and a model that does not accept it \
                     rejects the request.",
                "minimum": 1,
            },
            "thinking": {
                "type": "object",
                "description":
                    "Extended thinking, for models that expose it. \
                     `{\"type\":\"adaptive\"}` lets the model decide how much to think. \
                     Reasoning is never spoken aloud.",
            },
            "anthropic_beta": {
                "type": "array",
                "description":
                    "Beta features to opt into, for Anthropic models. Each entry is a \
                     feature name the model server recognises.",
            },
        },
    }))
    .expect("a literal object schema")
}

/// A language model served over the Bedrock runtime's Converse API.
///
/// Cloning is cheap and shares one client, which is what pooling connections
/// across turns requires.
///
/// Everything but the identity is gated: without the SDK there is no client to
/// hold and nothing else here has a reader, because a lean build refuses before
/// it ever constructs one of these.
#[derive(Debug, Clone)]
pub struct Bedrock {
    #[cfg(feature = "bedrock")]
    client: aws_sdk_bedrockruntime::Client,
    /// Region the client was built for, for diagnostics and error messages.
    #[cfg(feature = "bedrock")]
    region: String,
    descriptor: Descriptor,
    system_prompt: Option<String>,
    #[cfg(feature = "bedrock")]
    default_settings: serde_json::Map<String, serde_json::Value>,
}

impl Bedrock {
    /// Builds a provider from `config`.
    ///
    /// Async because resolving credentials is: the default chain reads the
    /// environment, then the shared config file, then — on an instance or a task
    /// — talks to a metadata endpoint over the network. Doing that once here
    /// rather than per turn is what keeps a conversation from paying for it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`](conduit_core::Error::Config) if this build was
    /// compiled without the `bedrock` feature, in which case there is no SDK to
    /// build a client with.
    #[cfg_attr(not(feature = "bedrock"), allow(unused_variables))]
    pub async fn new(config: BedrockConfig) -> Result<Self> {
        #[cfg(not(feature = "bedrock"))]
        {
            Err(conduit_core::Error::Config(format!(
                "provider `{}` talks to Amazon Bedrock, which this build cannot do: it was \
                 compiled without the `bedrock` feature",
                config.name
            )))
        }

        #[cfg(feature = "bedrock")]
        {
            let client = Self::client(&config).await;
            Ok(Self {
                client,
                region: config.region.clone(),
                descriptor: Self::descriptor(&config),
                system_prompt: config.system_prompt,
                default_settings: config.default_settings,
            })
        }
    }

    /// The identity, models, and settings schema this provider advertises.
    ///
    /// Built without touching the network, so it is the same whether or not the
    /// SDK is compiled in — an operator listing providers in a lean build sees
    /// the definition they configured rather than a gap.
    #[cfg_attr(not(feature = "bedrock"), allow(dead_code))]
    fn descriptor(config: &BedrockConfig) -> Descriptor {
        // An empty list is not an empty catalogue: it means the definition named
        // no models, so the current ones are advertised rather than leaving an
        // operator with nothing to choose from.
        let models = if config.models.is_empty() {
            crate::DEFAULT_MODELS.iter().map(|model| (*model).to_owned()).collect()
        } else {
            config.models.clone()
        };

        config
            .descriptor(Capability::Llm)
            .with_metadata(Metadata::default().with_models(models).with_tools())
            .with_settings(settings_schema())
    }
}

#[cfg(feature = "bedrock")]
impl Bedrock {
    /// Builds the runtime client `config` describes.
    ///
    /// Infallible on purpose. Every failure the SDK has here — no credentials,
    /// an unknown profile, an unreachable metadata endpoint — is one it reports
    /// when a request is *sent*, not when the client is built, and turning some
    /// of them into a build failure would mean a provider that cannot be
    /// registered and therefore cannot report its own health. An operator is
    /// better served by a provider that appears and says `Unhealthy` with the
    /// reason.
    async fn client(config: &BedrockConfig) -> aws_sdk_bedrockruntime::Client {
        aws_sdk_bedrockruntime::Client::new(&Self::loader(config).load().await)
    }

    /// The credential chain and transport `config` asks for, before it is loaded.
    ///
    /// Separated from [`Self::client`] so the credential decisions — which are
    /// the whole of what an operator configures here, and the part that fails
    /// most confusingly when it is wrong — can be asserted without an AWS
    /// account: loading this yields an `SdkConfig` whose region, timeouts, token
    /// provider, and auth scheme preference are all readable.
    fn loader(config: &BedrockConfig) -> aws_config::ConfigLoader {
        use aws_config::{BehaviorVersion, Region};
        use aws_smithy_http_client::tls;
        use aws_smithy_runtime_api::client::auth::http::HTTP_BEARER_AUTH_SCHEME_ID;

        let mut timeouts = aws_smithy_types::timeout::TimeoutConfig::builder()
            .connect_timeout(config.connect_timeout);
        // `None` means something above this already imposes a deadline, and the
        // SDK's own default read timeout would undercut it.
        timeouts.set_read_timeout(config.read_timeout);

        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            // The SDK's own `rustls` feature selects `aws-lc-rs`, which is C,
            // wants cmake, and would put a second crypto provider in a binary
            // that already links ring for every other provider. Built here
            // instead and handed over, so the whole workspace has one.
            .http_client(
                aws_smithy_http_client::Builder::new()
                    .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
                    .build_https(),
            )
            .timeout_config(timeouts.build());

        if let Some(profile) = &config.profile {
            loader = loader.profile_name(profile.clone());
        }
        if let Some(key) = &config.api_key {
            // A Bedrock API key is a bearer token, and the SDK will sign with
            // whatever the credential chain resolves unless bearer auth is
            // preferred explicitly. Left implicit, a key would be accepted and
            // then ignored in favour of an unrelated instance role.
            loader = loader
                .token_provider(aws_credential_types::Token::new(key.clone(), None))
                .auth_scheme_preference([HTTP_BEARER_AUTH_SCHEME_ID]);
        }

        loader
    }
}

#[async_trait::async_trait]
impl Provider for Bedrock {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        #[cfg(not(feature = "bedrock"))]
        {
            Health::Unhealthy {
                reason: "this build was compiled without the `bedrock` feature".to_owned(),
            }
        }

        #[cfg(feature = "bedrock")]
        {
            use aws_sdk_bedrockruntime::types::{
                ContentBlock, ConversationRole, ConverseTokensRequest, CountTokensInput,
                Message,
            };

            // The runtime endpoint has no list-models route and no unauthenticated
            // liveness one, so counting the tokens in a one-word conversation is
            // the cheapest call that exercises the credential, the region, and
            // the model id together. It runs no inference and is not billed as
            // such.
            let Some(model) = self.descriptor.metadata.models.first() else {
                return Health::Degraded {
                    reason: "no model to probe with: the definition advertises none".to_owned(),
                };
            };

            let Ok(message) = Message::builder()
                .role(ConversationRole::User)
                .content(ContentBlock::Text("ping".to_owned()))
                .build()
            else {
                return Health::Degraded {
                    reason: "could not build a probe request".to_owned(),
                };
            };

            let probe = self
                .client
                .count_tokens()
                .model_id(model)
                .input(CountTokensInput::Converse(
                    ConverseTokensRequest::builder().messages(message).build(),
                ))
                .send()
                .await;

            match probe {
                Ok(_) => Health::Healthy,
                Err(error) => Health::Unhealthy {
                    reason: format!(
                        "{} in {}",
                        crate::failure::of_count_tokens(&error),
                        self.region
                    ),
                },
            }
        }
    }
}

#[async_trait::async_trait]
impl LanguageModel for Bedrock {
    #[cfg_attr(not(feature = "bedrock"), allow(unused_variables))]
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream<Completion>> {
        #[cfg(not(feature = "bedrock"))]
        {
            Err(conduit_core::Error::Config(format!(
                "provider `{}` was compiled without the `bedrock` feature",
                self.descriptor.id
            )))
        }

        #[cfg(feature = "bedrock")]
        {
            let body = crate::wire::Request::from_completion(
                request,
                &self.default_settings,
                self.system_prompt.as_deref(),
            )
            .map_err(|error| {
                // Nothing was sent, and the reason is on this side: a request
                // the SDK will not assemble.
                conduit_core::Error::provider(
                    &self.descriptor.id,
                    conduit_http::Failure::unsendable(error.to_string()),
                )
            })?;

            tracing::debug!(
                model = %body.model,
                region = %self.region,
                tools = body.tools.as_ref().map_or(0, |config| config.tools().len()),
                "requesting completion"
            );

            let mut call = self
                .client
                .converse_stream()
                .model_id(&body.model)
                .set_messages(Some(body.messages))
                .set_system(Some(body.system))
                .inference_config(body.inference);
            if let Some(tools) = body.tools {
                call = call.tool_config(tools);
            }
            if let Some(additional) = body.additional {
                call = call.additional_model_request_fields(additional);
            }

            let response = call.send().await.map_err(|error| {
                conduit_core::Error::provider(
                    &self.descriptor.id,
                    crate::failure::of_request(&error),
                )
            })?;

            Ok(crate::stream::completions(response.stream, self.descriptor.id.clone()))
        }
    }

    fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor, without building a client.
    ///
    /// Every test here is about what the provider advertises, and none of it
    /// depends on reaching AWS — which is also why these tests run in a build
    /// without the feature.
    fn descriptor(config: BedrockConfig) -> Descriptor {
        Bedrock::descriptor(&config)
    }

    #[test]
    fn one_descriptor_answers_what_the_console_asks() {
        let descriptor = descriptor(BedrockConfig {
            name: "claude-bedrock".to_owned(),
            label: Some("Claude (Bedrock)".to_owned()),
            models: vec!["us.anthropic.claude-opus-4-5-20251101-v1:0".to_owned()],
            ..BedrockConfig::default()
        });

        assert_eq!(descriptor.id, "claude-bedrock");
        assert_eq!(descriptor.label, "Claude (Bedrock)");
        assert_eq!(descriptor.capability, Capability::Llm);
        assert_eq!(descriptor.metadata.models, ["us.anthropic.claude-opus-4-5-20251101-v1:0"]);
        assert!(descriptor.metadata.tools, "Converse calls tools");
        assert!(!descriptor.settings.is_empty(), "the request controls are declared");
    }

    #[test]
    fn naming_no_models_advertises_the_current_ones() {
        // An operator who has not typed a model list should still be offered
        // something to pick, rather than an empty menu.
        let descriptor = descriptor(BedrockConfig::default());

        assert_eq!(descriptor.metadata.models.len(), crate::DEFAULT_MODELS.len());
        assert!(
            descriptor.metadata.models.iter().all(|model| model.starts_with("us.")),
            "{:?}",
            descriptor.metadata.models
        );
    }

    #[test]
    fn a_label_falls_back_to_the_identity() {
        assert_eq!(descriptor(BedrockConfig::default()).label, "bedrock");
    }

    #[test]
    fn sampling_controls_belong_to_the_request_rather_than_to_the_settings() {
        // Converse takes `temperature` and `maxTokens` in a field of its own, so
        // naming them as settings would send them twice, in two places, with the
        // API deciding which won.
        let descriptor = descriptor(BedrockConfig::default());
        for rejected in ["temperature", "max_tokens", "maxTokens", "top_p"] {
            assert!(
                descriptor.validate_settings(&serde_json::json!({ rejected: 0.5 })).is_err(),
                "`{rejected}` is set from the request, not from the settings"
            );
        }
    }

    #[cfg(not(feature = "bedrock"))]
    #[tokio::test]
    async fn a_build_without_the_feature_refuses_by_name_rather_than_failing_a_turn() {
        // The point of the gate: an operator who deployed a lean binary and
        // configured Bedrock anyway learns which feature is missing when they
        // save the definition, not when someone speaks to it.
        let error = Bedrock::new(BedrockConfig::default())
            .await
            .expect_err("there is no SDK to build a client with");
        let message = error.to_string();

        assert!(message.contains("bedrock"), "{message}");
        assert!(message.contains("feature"), "the operator is told what to change: {message}");
    }

    /// What an operator configures about credentials, asserted without an
    /// account.
    ///
    /// Loading a [`aws_config::ConfigLoader`] resolves region, timeouts, and the
    /// auth decisions eagerly, but leaves the credential *chain* lazy — so these
    /// read the resolved configuration and never send a request. The one test
    /// that does resolve a credential resolves it from a file it wrote itself,
    /// which short-circuits the chain before the container and instance-metadata
    /// links that would reach the network.
    #[cfg(feature = "bedrock")]
    mod credentials {
        use super::*;
        use aws_smithy_runtime_api::client::auth::http::HTTP_BEARER_AUTH_SCHEME_ID;
        use aws_types::os_shim_internal::{Env, Fs};

        /// A config with nothing but a region, which is the shape a deployment
        /// holding its own role uses.
        fn in_region() -> BedrockConfig {
            BedrockConfig { region: "us-west-2".to_owned(), ..BedrockConfig::default() }
        }

        #[tokio::test]
        async fn a_named_profile_is_where_the_credential_comes_from() {
            // The named profile has to reach the loader for this to resolve at
            // all: the file holds a profile and a `default` that disagree, so a
            // loader that dropped `profile` would resolve the default's key and
            // this would fail with the wrong one rather than pass vacuously.
            //
            // `Env`/`Fs` overrides rather than the real environment, so nothing
            // on the machine running this is read and nothing is mutated — the
            // process-wide `set_var` alternative is `unsafe` and races the other
            // tests in this binary.
            let config = Bedrock::loader(&BedrockConfig {
                profile: Some("bedrock-operator".to_owned()),
                ..in_region()
            })
            .env(Env::from_slice(&[("HOME", "/home/operator")]))
            .fs(Fs::from_slice(&[(
                "/home/operator/.aws/credentials",
                "[default]\n\
                 aws_access_key_id = AKIAIOSFODNN7DEFAULT\n\
                 aws_secret_access_key = the-default-profiles\n\
                 \n\
                 [bedrock-operator]\n\
                 aws_access_key_id = AKIAIOSFODNN7NAMEDONE\n\
                 aws_secret_access_key = the-named-profiles\n",
            )]))
            .load()
            .await;

            let provider = config.credentials_provider().expect("the chain is configured");
            let resolved =
                aws_credential_types::provider::ProvideCredentials::provide_credentials(
                    &provider,
                )
                .await
                .expect("the named profile in the file this test supplied");

            assert_eq!(
                resolved.access_key_id(),
                "AKIAIOSFODNN7NAMEDONE",
                "the configured profile answered, not the default one"
            );
        }

        /// A syntactically plausible key that is not one. Never logged.
        const KEY: &str = "not-a-real-bedrock-api-key";

        #[tokio::test]
        async fn an_api_key_prefers_bearer_auth_rather_than_signing() {
            // The load-bearing one. Without the explicit preference the SDK
            // accepts the token and then signs with whatever the chain resolved
            // — an instance role the operator never named — so a configured key
            // is silently ignored and the failure points at the wrong thing.
            let config = Bedrock::loader(&BedrockConfig {
                api_key: Some(KEY.to_owned()),
                ..in_region()
            })
            // An empty environment: the preference is also readable from
            // `AWS_AUTH_SCHEME_PREFERENCE`, and the claim under test is
            // that the *definition* sets it.
            .env(Env::from_slice(&[]))
            .fs(Fs::from_slice(&[]))
            .load()
            .await;

            let preference = config
                .auth_scheme_preference()
                .expect("a key without a stated preference would be signed over");
            assert!(
                preference
                    .clone()
                    .into_iter()
                    .any(|scheme| scheme == HTTP_BEARER_AUTH_SCHEME_ID),
                "bearer auth is preferred"
            );
            // The default chain installs a token provider of its own for SSO, so
            // its mere presence proves nothing; what proves the key was carried
            // is that resolving the token yields this one. Compared, never
            // logged.
            let token = aws_credential_types::provider::token::ProvideToken::provide_token(
                &config.token_provider().expect("a token provider"),
            )
            .await
            .expect("the key this test configured");
            assert!(token.token() == KEY, "the configured key is what would be sent");
        }

        #[tokio::test]
        async fn omitting_both_leaves_the_default_chain_to_answer() {
            // Neither a profile nor a key: the deployment has an identity of its
            // own — a task role, an instance profile, an SSO session — and the
            // chain must be left to find it and to sign, which is what saying
            // nothing about the auth scheme means.
            let config = Bedrock::loader(&in_region())
                .env(Env::from_slice(&[]))
                .fs(Fs::from_slice(&[]))
                .load()
                .await;

            assert!(
                config.auth_scheme_preference().is_none(),
                "nothing overrides SigV4, so the chain's credential signs"
            );
            assert!(config.credentials_provider().is_some(), "the default chain is installed");
            assert_eq!(config.region().map(aws_config::Region::as_ref), Some("us-west-2"));
        }
    }

    #[test]
    fn a_model_specific_control_is_a_declared_setting() {
        let descriptor = descriptor(BedrockConfig::default());

        assert!(
            descriptor.validate_settings(&serde_json::json!({ "top_k": 40 })).is_ok(),
            "`top_k` travels as an additional model request field"
        );
        assert!(descriptor
            .validate_settings(&serde_json::json!({ "thinking": { "type": "adaptive" } }))
            .is_ok());
    }
}
