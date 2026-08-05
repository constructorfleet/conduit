//! The Google Cloud vendor: synthesis and recognition.
//!
//! The only factory here whose definitions carry no credential. Nobody types a
//! Google key: credentials are discovered from the metadata server, from the
//! service-account JSON `GOOGLE_APPLICATION_CREDENTIALS` names, or from whatever
//! `gcloud auth application-default login` last wrote. So there is no
//! [`secret_value`] call in this file, and there is nothing on the form for an
//! operator to paste.
//!
//! That discovery happens while the definition is being built, which is why both
//! constructors are async: a deployment with no credentials is told so when it
//! saves the definition rather than the first time somebody speaks.
//!
//! [`secret_value`]: super::secret_value

use conduit_core::Result;
use conduit_google::{GoogleConfig, GoogleStt, GoogleTts, DEFAULT_LANGUAGE};
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, SttVariant, TtsVariant,
};
use conduit_runtime::Providers;

use super::{unclaimed, ProviderFactory};

/// Providers reached over the Google Cloud speech APIs.
///
/// One factory for both capabilities because they are one credential: a
/// deployment authorizes itself to Google once, not once per capability.
pub struct Google;

#[async_trait::async_trait]
impl ProviderFactory for Google {
    fn name(&self) -> &'static str {
        "google"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Stt { variant: SttVariant::Google { .. } }
                | ProviderDefinitionVariant::Tts { variant: TtsVariant::Google { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        match &definition.variant {
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::Google { language, model },
            } => {
                let mut config = config(definition, language.as_deref());
                config.model = model.clone();
                Ok(providers.with_stt(GoogleStt::new(&config).await?))
            }
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Google { language, voice },
            } => {
                let mut config = config(definition, language.as_deref());
                config.voice = voice.clone();
                Ok(providers.with_tts(GoogleTts::new(&config).await?))
            }
            _ => Err(unclaimed(self.name(), definition)),
        }
    }
}

/// The credential and its settings, as both capabilities want them.
///
/// A language is always sent: Google requires `languageCode` on every synthesis
/// request and has no server-side default for it, so a definition that names
/// none gets the documented default rather than a 400.
fn config(definition: &ProviderDefinition, language: Option<&str>) -> GoogleConfig {
    GoogleConfig {
        name: definition.id.clone(),
        // The label an operator typed, kept distinct from the identity: the
        // provider registers under the definition id and calls itself by it, and
        // this is only what a screen shows.
        label: Some(definition.label.clone()),
        language: language.unwrap_or(DEFAULT_LANGUAGE).to_owned(),
        // Already checked against the provider's declared schema when the
        // definition was stored. Every request through it starts from these.
        default_settings: definition.settings.clone(),
        ..GoogleConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tts(language: Option<&str>, voice: Option<&str>) -> ProviderDefinition {
        ProviderDefinition {
            id: "google-voice".to_owned(),
            label: "Google Voice".to_owned(),
            variant: ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Google {
                    language: language.map(str::to_owned),
                    voice: voice.map(str::to_owned),
                },
            },
            settings: Default::default(),
        }
    }

    fn stt() -> ProviderDefinition {
        ProviderDefinition {
            id: "google-ears".to_owned(),
            label: "Google Ears".to_owned(),
            variant: ProviderDefinitionVariant::Stt {
                variant: SttVariant::Google {
                    language: Some("en-GB".to_owned()),
                    model: Some("latest_long".to_owned()),
                },
            },
            settings: Default::default(),
        }
    }

    #[test]
    fn both_google_definitions_are_claimed_and_nothing_else_is() {
        assert!(Google.handles(&tts(None, None)));
        assert!(Google.handles(&stt()));

        let mut other = stt();
        other.variant = ProviderDefinitionVariant::Stt {
            variant: SttVariant::ElevenLabs { api_key: None, model: None },
        };
        assert!(!Google.handles(&other), "the ElevenLabs factory's, not this one's");
    }

    #[tokio::test]
    async fn a_definition_this_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree. If they ever disagree, the
        // definition is reported rather than silently producing no provider.
        // Refused before credentials are resolved, so this does not need any.
        let mut definition = stt();
        definition.variant = ProviderDefinitionVariant::Stt {
            variant: SttVariant::Wyoming {
                url: "tcp://whisper:10300".to_owned(),
                model: None,
                streaming: false,
            },
        };

        let error = Google.register(Providers::new(), &definition).await.expect_err("not ours");
        assert!(error.to_string().contains("google-ears"), "{error}");
    }

    #[test]
    fn a_definition_that_names_no_language_gets_the_documented_default() {
        // Not an empty string: Google rejects a synthesis request with no
        // `languageCode`, and it has no server-side default to fall back on.
        assert_eq!(config(&tts(None, None), None).language, "en-US");
    }

    #[test]
    fn a_configured_language_reaches_the_client() {
        assert_eq!(config(&stt(), Some("en-GB")).language, "en-GB");
    }

    #[test]
    fn the_definition_supplies_the_identity_and_the_label_separately() {
        let config = config(&tts(None, None), None);

        assert_eq!(config.name, "google-voice", "the id it registers under");
        assert_eq!(config.label, Some("Google Voice".to_owned()), "only what a screen shows");
    }

    #[test]
    fn stored_settings_reach_the_provider_as_its_defaults() {
        let mut definition = tts(None, None);
        definition.settings.insert("pitch".to_owned(), serde_json::json!(-2.0));

        let config = config(&definition, None);
        assert_eq!(config.default_settings.get("pitch"), Some(&serde_json::json!(-2.0)));
    }
}
