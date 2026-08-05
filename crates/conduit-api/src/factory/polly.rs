//! The Amazon Polly vendor: synthesis named by region.
//!
//! Separate from the OpenAI-compatible speech factory for the reason
//! `BedrockRuntime` is separate from `Anthropic`: a definition here names a region
//! and leans on whatever credential the deployment already has, rather than naming
//! a URL and a key.
//!
//! There is no `secret_value` call anywhere in this file, and that is the point.
//! Polly has no API key, so there is nothing to unwrap, nothing to redact on the
//! way out, and no `Redacted` placeholder that could reach the vendor.

use conduit_core::Result;
use conduit_polly::{PollyTts, PollyTtsConfig};
use conduit_provider::storage::{ProviderDefinition, ProviderDefinitionVariant, TtsVariant};
use conduit_runtime::Providers;

use super::{unclaimed, ProviderFactory};

/// Synthesis served by Amazon Polly.
pub struct Polly;

#[async_trait::async_trait]
impl ProviderFactory for Polly {
    fn name(&self) -> &'static str {
        "polly"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Tts { variant: TtsVariant::Polly { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Tts {
            variant: TtsVariant::Polly { region, profile, voice, engine },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };

        Ok(providers.with_tts(
            PollyTts::new(PollyTtsConfig {
                name: definition.id.clone(),
                // The label an operator typed, kept distinct from the identity:
                // the provider registers under the definition id, and this is
                // only what a screen shows.
                label: Some(definition.label.clone()),
                region: region.clone(),
                profile: profile.clone(),
                voice: voice.clone(),
                engine: engine.clone(),
                ..PollyTtsConfig::default()
            })
            .await?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(voice: Option<&str>, engine: Option<&str>) -> ProviderDefinition {
        ProviderDefinition {
            id: "house-voice".to_owned(),
            label: "House (Polly)".to_owned(),
            variant: ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Polly {
                    region: "us-west-2".to_owned(),
                    profile: None,
                    voice: voice.map(str::to_owned),
                    engine: engine.map(str::to_owned),
                },
            },
            settings: Default::default(),
        }
    }

    #[cfg(feature = "polly")]
    #[tokio::test]
    async fn a_stored_definition_becomes_a_voice_under_its_own_id() {
        let providers = Polly
            .register(Providers::new(), &definition(Some("Matthew"), Some("generative")))
            .await
            .expect("builds without reaching AWS");

        assert_eq!(providers.tts().names().collect::<Vec<_>>(), ["house-voice"]);
        let voice = providers.tts().get("house-voice").expect("registered");
        assert_eq!(voice.descriptor().label, "House (Polly)");
        assert_eq!(voice.descriptor().metadata.voices[0].id, "Matthew");
    }

    #[cfg(feature = "polly")]
    #[tokio::test]
    async fn a_definition_naming_only_a_region_still_registers_a_working_voice() {
        // The common definition: a task role supplies the credential, so a region
        // is the whole of what an operator has to type.
        let providers =
            Polly.register(Providers::new(), &definition(None, None)).await.expect("builds");

        let voice = providers.tts().get("house-voice").expect("registered");
        assert_eq!(voice.descriptor().metadata.voices[0].id, conduit_polly::DEFAULT_VOICE);
    }

    #[test]
    fn a_polly_definition_is_claimed_and_nothing_else_is() {
        assert!(Polly.handles(&definition(None, None)));

        let mut other = definition(None, None);
        other.variant = ProviderDefinitionVariant::Tts {
            variant: TtsVariant::Google { language: None, voice: None },
        };
        assert!(!Polly.handles(&other), "the Google factory's, not this one's");
    }

    #[tokio::test]
    async fn a_definition_this_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree; if they ever disagree, the
        // definition is reported rather than silently producing no provider.
        let mut refused = definition(None, None);
        refused.variant = ProviderDefinitionVariant::Tts {
            variant: TtsVariant::ElevenLabs { api_key: None, model: None, voice: None },
        };

        let error = Polly.register(Providers::new(), &refused).await.expect_err("not ours");
        assert!(error.to_string().contains("house-voice"), "{error}");
    }

    #[cfg(not(feature = "polly"))]
    #[tokio::test]
    async fn without_the_feature_the_definition_is_still_claimed_and_the_refusal_names_it() {
        // Claimed, not ignored: an unclaimed definition reads as a typo in the
        // variant, and this one is spelled correctly — the build simply cannot
        // serve it.
        let definition = definition(None, None);
        assert!(Polly.handles(&definition));

        let error =
            Polly.register(Providers::new(), &definition).await.expect_err("no AWS SDK here");
        assert!(error.to_string().contains("polly"), "names the feature: {error}");
    }
}
