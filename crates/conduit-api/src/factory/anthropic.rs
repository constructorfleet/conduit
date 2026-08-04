//! The Anthropic vendor: reasoning over the Messages API.

use conduit_anthropic::{Anthropic as AnthropicLlm, AnthropicConfig};
use conduit_core::Result;
use conduit_provider::storage::{LlmVariant, ProviderDefinition, ProviderDefinitionVariant};
use conduit_runtime::Providers;

use super::{secret_value, unclaimed, ProviderFactory};

/// Language models reached over Anthropic's Messages API.
///
/// One capability, unlike the OpenAI factory's three, because this API serves
/// one: there is no transcription or synthesis route to configure alongside it.
pub struct Anthropic;

#[async_trait::async_trait]
impl ProviderFactory for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Llm { variant: LlmVariant::Anthropic { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Llm {
            variant: LlmVariant::Anthropic { base_url, api_key, models, system_prompt, .. },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };

        let mut config = config(definition, base_url, api_key);
        config.models = models.clone();
        config.system_prompt = system_prompt.clone();
        Ok(providers.with_llm(AnthropicLlm::new(config)?))
    }
}

/// How a definition says to reach the API.
fn config(
    definition: &ProviderDefinition,
    base_url: &str,
    api_key: &Option<conduit_provider::storage::ProviderSecret>,
) -> AnthropicConfig {
    AnthropicConfig {
        base_url: base_url.to_owned(),
        api_key: secret_value(api_key),
        name: definition.id.clone(),
        // The label an operator typed, kept distinct from the identity: the
        // provider registers under the definition id and calls itself by it,
        // and this is only what a screen shows.
        label: Some(definition.label.clone()),
        // Already checked against the provider's declared schema when the
        // definition was stored, which is where a setting this API rejects —
        // `temperature`, say — was refused.
        default_settings: definition.settings.clone(),
        ..AnthropicConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_provider::storage::ProviderSecret;

    fn definition(api_key: Option<ProviderSecret>) -> ProviderDefinition {
        ProviderDefinition {
            id: "claude".to_owned(),
            label: "Claude (house)".to_owned(),
            variant: ProviderDefinitionVariant::Llm {
                variant: LlmVariant::Anthropic {
                    base_url: "https://api.anthropic.com/v1".to_owned(),
                    api_key,
                    models: vec!["claude-opus-5".to_owned()],
                    streaming: true,
                    system_prompt: Some("Be terse.".to_owned()),
                },
            },
            settings: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_stored_definition_becomes_a_model_under_its_own_id() {
        let providers = Anthropic
            .register(
                Providers::new(),
                &definition(Some(ProviderSecret::Inline { value: "sk-ant-test".to_owned() })),
            )
            .await
            .expect("builds");

        assert_eq!(providers.llm().names().collect::<Vec<_>>(), ["claude"]);
        let model = providers.llm().get("claude").expect("registered");
        assert_eq!(model.descriptor().label, "Claude (house)");
        assert_eq!(model.descriptor().metadata.models, ["claude-opus-5"]);
    }

    #[tokio::test]
    async fn a_definition_this_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree. If they ever disagree, the
        // definition is reported rather than silently producing no provider.
        let mut definition = definition(None);
        definition.variant = ProviderDefinitionVariant::Llm {
            variant: LlmVariant::OpenAi {
                base_url: "https://api.openai.com/v1".to_owned(),
                api_key: None,
                models: Vec::new(),
                streaming: true,
                system_prompt: None,
            },
        };

        let error =
            Anthropic.register(Providers::new(), &definition).await.expect_err("not ours");
        assert!(error.to_string().contains("claude"), "{error}");
    }

    #[test]
    fn a_key_this_process_does_not_hold_is_not_sent_as_one() {
        // A read response carries `Redacted`, and a definition round-tripped
        // through the console can arrive holding one. An external reference is
        // resolved by whatever manages it. Either sent as if it were the key
        // would authenticate with a placeholder.
        for absent in [
            None,
            Some(ProviderSecret::Redacted),
            Some(ProviderSecret::External { reference: "vault://claude".to_owned() }),
        ] {
            let definition = definition(absent.clone());
            let config = config(&definition, "https://api.anthropic.com/v1", &absent);

            assert_eq!(config.api_key, None, "{absent:?} is not a key to send");
        }
    }

    #[test]
    fn a_key_this_process_holds_reaches_the_client() {
        let definition = definition(None);
        let inline = Some(ProviderSecret::Inline { value: "sk-ant-live".to_owned() });

        let config = config(&definition, "https://api.anthropic.com/v1", &inline);

        assert_eq!(config.api_key, Some("sk-ant-live".to_owned()));
    }
}
