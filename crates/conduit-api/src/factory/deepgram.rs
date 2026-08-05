//! The Deepgram vendor: Aura synthesis over `/v1/speak`.
//!
//! A definition names a key and, optionally, a voice. There is no URL, because
//! there is one Deepgram — and the key is sent as `Authorization: Token`, which
//! is the detail that makes this a factory rather than a preset.

use conduit_core::Result;
use conduit_deepgram::{DeepgramTts, DeepgramTtsConfig};
use conduit_provider::storage::{ProviderDefinition, ProviderDefinitionVariant, TtsVariant};
use conduit_runtime::Providers;

use super::{secret_value, unclaimed, ProviderFactory};

/// Synthesis served by Deepgram Aura.
pub struct Deepgram;

#[async_trait::async_trait]
impl ProviderFactory for Deepgram {
    fn name(&self) -> &'static str {
        "deepgram"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Tts { variant: TtsVariant::Deepgram { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Tts { variant: TtsVariant::Deepgram { api_key, model } } =
            &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };

        Ok(providers.with_tts(DeepgramTts::new(DeepgramTtsConfig {
            name: definition.id.clone(),
            // The label an operator typed, kept distinct from the identity: the
            // provider registers under the definition id, and this is only what
            // a screen shows.
            label: Some(definition.label.clone()),
            api_key: secret_value(api_key),
            model: model.clone(),
            ..DeepgramTtsConfig::default()
        })?))
    }
}

#[cfg(test)]
mod tests {
    use conduit_provider::storage::ProviderSecret;

    use super::*;

    fn definition(api_key: Option<ProviderSecret>, model: Option<&str>) -> ProviderDefinition {
        ProviderDefinition {
            id: "house-voice".to_owned(),
            label: "House (Deepgram)".to_owned(),
            variant: ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Deepgram { api_key, model: model.map(str::to_owned) },
            },
            settings: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_stored_definition_becomes_a_voice_under_its_own_id() {
        let providers = Deepgram
            .register(
                Providers::new(),
                &definition(
                    Some(ProviderSecret::Inline { value: "dg-key".to_owned() }),
                    Some("aura-2-thalia-en"),
                ),
            )
            .await
            .expect("builds without reaching the API");

        assert_eq!(providers.tts().names().collect::<Vec<_>>(), ["house-voice"]);
        let voice = providers.tts().get("house-voice").expect("registered");
        assert_eq!(voice.descriptor().label, "House (Deepgram)");
        // The voice catalogue is the model, so a configured model is what an
        // operator screen offers.
        assert_eq!(voice.descriptor().metadata.voices[0].id, "aura-2-thalia-en");
    }

    #[tokio::test]
    async fn a_definition_naming_no_voice_still_registers() {
        // Deepgram has its own default, so a key alone is a working definition.
        let providers = Deepgram
            .register(
                Providers::new(),
                &definition(Some(ProviderSecret::Inline { value: "dg".to_owned() }), None),
            )
            .await
            .expect("builds");

        let voice = providers.tts().get("house-voice").expect("registered");
        assert_eq!(voice.descriptor().metadata.voices[0].id, "aura-asteria-en");
    }

    #[tokio::test]
    async fn a_redacted_key_does_not_travel_to_the_vendor() {
        // What a read response hands back is a placeholder, not a credential.
        // Registering with it must not send the string "redacted" as a key.
        let providers = Deepgram
            .register(Providers::new(), &definition(Some(ProviderSecret::Redacted), None))
            .await
            .expect("builds");

        let voice = providers.tts().get("house-voice").expect("registered");
        assert!(!format!("{:?}", voice.descriptor()).contains("edacted"));
    }

    #[test]
    fn a_deepgram_definition_is_claimed_and_nothing_else_is() {
        assert!(Deepgram.handles(&definition(None, None)));

        let mut other = definition(None, None);
        other.variant = ProviderDefinitionVariant::Tts {
            variant: TtsVariant::ElevenLabs { api_key: None, model: None, voice: None },
        };
        assert!(!Deepgram.handles(&other), "the ElevenLabs factory's, not this one's");
    }

    #[tokio::test]
    async fn a_definition_this_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree; if they ever disagree, the
        // definition is reported rather than silently producing no provider.
        let mut refused = definition(None, None);
        refused.variant = ProviderDefinitionVariant::Tts {
            variant: TtsVariant::Google { language: None, voice: None },
        };

        let error = Deepgram.register(Providers::new(), &refused).await.expect_err("not ours");
        assert!(error.to_string().contains("house-voice"), "{error}");
    }
}
