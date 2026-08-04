//! The Amazon Bedrock vendor: reasoning over the Converse API.
//!
//! Separate from the Anthropic factory even though both usually serve the same
//! models, because a definition says something different in each: one names a URL
//! and a key, the other names a region and leans on whatever credential the
//! deployment already has. What they share is the vendor's models, not the way to
//! reach them.

use conduit_bedrock::{Bedrock as BedrockLlm, BedrockConfig};
use conduit_core::Result;
use conduit_provider::storage::{LlmVariant, ProviderDefinition, ProviderDefinitionVariant};
use conduit_runtime::Providers;

use super::{secret_value, unclaimed, ProviderFactory};

/// Language models reached over Amazon Bedrock's Converse API.
pub struct Bedrock;

#[async_trait::async_trait]
impl ProviderFactory for Bedrock {
    fn name(&self) -> &'static str {
        "bedrock"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Llm { variant: LlmVariant::Bedrock { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Llm {
            variant: LlmVariant::Bedrock { region, profile, api_key, models, system_prompt, .. },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };

        let mut config = config(definition, region, profile, api_key);
        config.models = models.clone();
        config.system_prompt = system_prompt.clone();
        Ok(providers.with_llm(BedrockLlm::new(config).await?))
    }
}

/// How a definition says to reach the runtime.
fn config(
    definition: &ProviderDefinition,
    region: &str,
    profile: &Option<String>,
    api_key: &Option<conduit_provider::storage::ProviderSecret>,
) -> BedrockConfig {
    BedrockConfig {
        region: region.to_owned(),
        profile: profile.clone(),
        api_key: secret_value(api_key),
        name: definition.id.clone(),
        // The label an operator typed, kept distinct from the identity: the
        // provider registers under the definition id and calls itself by it, and
        // this is only what a screen shows.
        label: Some(definition.label.clone()),
        // Already checked against the provider's declared schema when the
        // definition was stored, which is where a setting Converse takes from the
        // request instead — `temperature`, say — was refused.
        default_settings: definition.settings.clone(),
        ..BedrockConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_provider::storage::ProviderSecret;

    fn definition(api_key: Option<ProviderSecret>) -> ProviderDefinition {
        ProviderDefinition {
            id: "claude-bedrock".to_owned(),
            label: "Claude (Bedrock)".to_owned(),
            variant: ProviderDefinitionVariant::Llm {
                variant: LlmVariant::Bedrock {
                    region: "us-west-2".to_owned(),
                    profile: None,
                    api_key,
                    models: vec!["us.anthropic.claude-opus-4-5-20251101-v1:0".to_owned()],
                    streaming: true,
                    system_prompt: Some("Be terse.".to_owned()),
                },
            },
            settings: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_stored_definition_becomes_a_model_under_its_own_id() {
        let providers =
            Bedrock.register(Providers::new(), &definition(None)).await.expect("builds");

        assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["claude-bedrock"]);
        let model = providers.llm().get("claude-bedrock").expect("registered");
        assert_eq!(model.descriptor().label, "Claude (Bedrock)");
        assert_eq!(
            model.descriptor().metadata.models,
            ["us.anthropic.claude-opus-4-5-20251101-v1:0"]
        );
    }

    #[test]
    fn a_bedrock_definition_is_claimed_and_nothing_else_is() {
        assert!(Bedrock.handles(&definition(None)));

        let mut other = definition(None);
        other.variant = ProviderDefinitionVariant::Llm {
            variant: LlmVariant::Anthropic {
                base_url: "https://api.anthropic.com/v1".to_owned(),
                api_key: None,
                models: Vec::new(),
                streaming: true,
                system_prompt: None,
            },
        };
        assert!(!Bedrock.handles(&other), "the Anthropic factory's, not this one's");
    }

    #[tokio::test]
    async fn a_definition_this_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree. If they ever disagree, the
        // definition is reported rather than silently producing no provider.
        let mut definition = definition(None);
        definition.variant = ProviderDefinitionVariant::Llm {
            variant: LlmVariant::Anthropic {
                base_url: "https://api.anthropic.com/v1".to_owned(),
                api_key: None,
                models: Vec::new(),
                streaming: true,
                system_prompt: None,
            },
        };

        let error =
            Bedrock.register(Providers::new(), &definition).await.expect_err("not ours");
        assert!(error.to_string().contains("claude-bedrock"), "{error}");
    }

    #[test]
    fn a_definition_that_names_no_credential_leaves_it_to_the_environment() {
        // The common deployment: a task role or an instance profile already
        // supplies one, and inventing an empty key would override it.
        let definition = definition(None);
        let config = config(&definition, "us-west-2", &None, &None);

        assert_eq!(config.api_key, None);
        assert_eq!(config.profile, None, "the default chain");
        assert_eq!(config.region, "us-west-2");
    }

    #[test]
    fn a_named_profile_reaches_the_client() {
        let definition = definition(None);
        let profile = Some("voice".to_owned());

        let config = config(&definition, "us-west-2", &profile, &None);

        assert_eq!(config.profile, Some("voice".to_owned()));
    }

    #[test]
    fn a_key_this_process_does_not_hold_is_not_sent_as_one() {
        // A read response carries `Redacted`, and a definition round-tripped
        // through the console can arrive holding one. An external reference is
        // resolved by whatever manages it. Either sent as if it were the key
        // would authenticate with a placeholder — and here it would also
        // override a working credential the environment supplies.
        for absent in [
            None,
            Some(ProviderSecret::Redacted),
            Some(ProviderSecret::External { reference: "vault://bedrock".to_owned() }),
        ] {
            let definition = definition(absent.clone());
            let config = config(&definition, "us-west-2", &None, &absent);

            assert_eq!(config.api_key, None, "{absent:?} is not a key to send");
        }
    }

    #[test]
    fn a_key_this_process_holds_reaches_the_client() {
        let definition = definition(None);
        let inline = Some(ProviderSecret::Inline { value: "ABSKbedrock".to_owned() });

        let config = config(&definition, "us-west-2", &None, &inline);

        assert_eq!(config.api_key, Some("ABSKbedrock".to_owned()));
    }
}
