//! The ElevenLabs vendor: synthesis and batch transcription.
//!
//! Not a base URL under the OpenAI factory, even though both vendors serve
//! speech over HTTP: the credential travels in a header of its own, the voice is
//! a URL path segment, and the output format is a query parameter. See
//! `conduit-elevenlabs` for the full list of differences.

use conduit_core::Result;
use conduit_elevenlabs::{ElevenLabsConfig, ElevenLabsStt, ElevenLabsTts};
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, ProviderSecret, SttVariant, TtsVariant,
};
use conduit_runtime::Providers;

use super::{secret_value, unclaimed, ProviderFactory};

/// Providers reached over the ElevenLabs API.
///
/// One factory for both capabilities because they are one account: a key and a
/// base URL, configured once.
pub struct ElevenLabs;

#[async_trait::async_trait]
impl ProviderFactory for ElevenLabs {
    fn name(&self) -> &'static str {
        "elevenlabs"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Stt { variant: SttVariant::ElevenLabs { .. } }
                | ProviderDefinitionVariant::Tts { variant: TtsVariant::ElevenLabs { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        match &definition.variant {
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::ElevenLabs { api_key, model },
            } => {
                let mut config = config(definition, api_key);
                config.models = model.iter().cloned().collect();
                Ok(providers.with_stt(ElevenLabsStt::new(&config)?))
            }
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::ElevenLabs { api_key, model, voice },
            } => {
                let mut config = config(definition, api_key);
                config.models = model.iter().cloned().collect();
                // The voice is checked as a path segment inside the provider,
                // which is why a stored definition carrying a traversal attempt
                // fails to build here rather than on the first turn.
                config.voice_id = voice.clone();
                Ok(providers.with_tts(ElevenLabsTts::new(&config)?))
            }
            _ => Err(unclaimed(self.name(), definition)),
        }
    }
}

/// The account configuration both capabilities share.
fn config(
    definition: &ProviderDefinition,
    api_key: &Option<ProviderSecret>,
) -> ElevenLabsConfig {
    ElevenLabsConfig {
        api_key: secret_value(api_key),
        name: definition.id.clone(),
        // The label an operator typed, kept distinct from the identity: the
        // provider registers under the definition id and calls itself by it,
        // and this is only what a screen shows.
        label: Some(definition.label.clone()),
        // Already checked against the provider's declared schema when the
        // definition was stored. Every request through it starts from these.
        default_settings: definition.settings.clone(),
        ..ElevenLabsConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tts(voice: Option<&str>) -> ProviderDefinition {
        ProviderDefinition {
            id: "house-voice".to_owned(),
            label: "House Voice".to_owned(),
            variant: ProviderDefinitionVariant::Tts {
                variant: TtsVariant::ElevenLabs {
                    api_key: Some(ProviderSecret::Inline { value: "sk_test".to_owned() }),
                    model: Some("eleven_flash_v2_5".to_owned()),
                    voice: voice.map(str::to_owned),
                },
            },
            settings: Default::default(),
        }
    }

    fn stt() -> ProviderDefinition {
        ProviderDefinition {
            id: "scribe".to_owned(),
            label: "Scribe".to_owned(),
            variant: ProviderDefinitionVariant::Stt {
                variant: SttVariant::ElevenLabs {
                    api_key: None,
                    model: Some("scribe_v2".to_owned()),
                },
            },
            settings: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_stored_definition_becomes_a_voice_under_its_own_id() {
        let providers = ElevenLabs
            .register(Providers::new(), &tts(Some("21m00Tcm4TlvDq8ikWAM")))
            .await
            .expect("builds");

        assert_eq!(providers.tts().names().collect::<Vec<_>>(), ["house-voice"]);
        let voice = providers.tts().get("house-voice").expect("registered");
        assert_eq!(voice.descriptor().label, "House Voice");
        assert_eq!(voice.descriptor().metadata.models, ["eleven_flash_v2_5"]);
    }

    #[tokio::test]
    async fn a_stored_definition_becomes_a_recognizer_under_its_own_id() {
        let providers = ElevenLabs.register(Providers::new(), &stt()).await.expect("builds");

        assert_eq!(providers.stt().names().collect::<Vec<_>>(), ["scribe"]);
        assert_eq!(
            providers.stt().get("scribe").expect("registered").descriptor().metadata.models,
            ["scribe_v2"]
        );
    }

    #[tokio::test]
    async fn a_voice_that_cannot_be_a_path_segment_fails_the_definition_not_the_turn() {
        // The voice reaches a URL path with the account's credential attached,
        // so a definition carrying a traversal attempt must refuse to become a
        // provider while an operator is still looking at the form.
        let error = ElevenLabs
            .register(Providers::new(), &tts(Some("../../admin")))
            .await
            .expect_err("not a path segment");

        assert!(error.to_string().contains("voice"), "{error}");
    }

    #[test]
    fn both_elevenlabs_definitions_are_claimed_and_nothing_else_is() {
        assert!(ElevenLabs.handles(&tts(None)));
        assert!(ElevenLabs.handles(&stt()));

        let mut other = stt();
        other.variant = ProviderDefinitionVariant::Stt {
            variant: SttVariant::OpenAi {
                base_url: "https://api.openai.com/v1".to_owned(),
                model: "whisper-1".to_owned(),
                api_key: None,
                stream: false,
            },
        };
        assert!(!ElevenLabs.handles(&other), "the OpenAI factory's, not this one's");
    }

    #[tokio::test]
    async fn a_definition_this_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree. If they ever disagree, the
        // definition is reported rather than silently producing no provider.
        let mut definition = stt();
        definition.variant = ProviderDefinitionVariant::Stt {
            variant: SttVariant::Wyoming {
                url: "tcp://whisper:10300".to_owned(),
                model: None,
                streaming: false,
            },
        };

        let error =
            ElevenLabs.register(Providers::new(), &definition).await.expect_err("not ours");
        assert!(error.to_string().contains("scribe"), "{error}");
    }

    #[test]
    fn a_key_this_process_does_not_hold_is_not_sent_as_one() {
        // A read response carries `Redacted`, and a definition round-tripped
        // through the console can arrive holding one. Sent as if it were the key
        // it would authenticate with a placeholder and 401 on every turn.
        for absent in [
            None,
            Some(ProviderSecret::Redacted),
            Some(ProviderSecret::External { reference: "vault://xi".to_owned() }),
        ] {
            assert_eq!(
                config(&stt(), &absent).api_key,
                None,
                "{absent:?} is not a key to send"
            );
        }
    }

    #[test]
    fn a_key_this_process_holds_reaches_the_client() {
        let held = Some(ProviderSecret::Inline { value: "sk_live".to_owned() });
        assert_eq!(config(&stt(), &held).api_key, Some("sk_live".to_owned()));
    }
}
