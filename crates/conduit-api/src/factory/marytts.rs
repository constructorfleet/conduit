//! The MaryTTS vendor: synthesis by a self-hosted server.
//!
//! There is no credential and no account. A definition names a URL, and that is
//! the whole configuration — which is the point of the provider: it is what a
//! deployment uses when it has decided its users' speech does not leave the
//! building.

use conduit_core::Result;
use conduit_marytts::{MaryTts, MaryTtsConfig};
use conduit_provider::storage::{ProviderDefinition, ProviderDefinitionVariant, TtsVariant};
use conduit_runtime::Providers;

use super::{unclaimed, ProviderFactory};

/// Synthesis served by a MaryTTS server.
pub struct MaryTtsServer;

#[async_trait::async_trait]
impl ProviderFactory for MaryTtsServer {
    fn name(&self) -> &'static str {
        "marytts"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Tts { variant: TtsVariant::MaryTts { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Tts {
            variant: TtsVariant::MaryTts { url, voice, locale },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };

        Ok(providers.with_tts(MaryTts::new(config(definition, url, voice, locale))?))
    }
}

/// How a definition says to reach the server.
///
/// A definition that names no locale gets the crate's default rather than an
/// empty one, because `/process` requires a locale whenever no voice is named.
fn config(
    definition: &ProviderDefinition,
    url: &str,
    voice: &Option<String>,
    locale: &Option<String>,
) -> MaryTtsConfig {
    let default = MaryTtsConfig::default();
    MaryTtsConfig {
        base_url: url.to_owned(),
        name: definition.id.clone(),
        // The label an operator typed, kept distinct from the identity: the
        // provider registers under the definition id and calls itself by it, and
        // this is only what a screen shows.
        label: Some(definition.label.clone()),
        voice: voice.clone(),
        locale: locale.clone().unwrap_or(default.locale),
        ..default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(voice: Option<&str>, locale: Option<&str>) -> ProviderDefinition {
        ProviderDefinition {
            id: "house".to_owned(),
            label: "House (MaryTTS)".to_owned(),
            variant: ProviderDefinitionVariant::Tts {
                variant: TtsVariant::MaryTts {
                    url: "http://marytts.lan:59125".to_owned(),
                    voice: voice.map(str::to_owned),
                    locale: locale.map(str::to_owned),
                },
            },
            settings: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_stored_definition_becomes_a_voice_under_its_own_id() {
        // Construction does not reach the server, so this builds with nothing
        // listening — which is what lets a definition be saved before the server
        // is up.
        let providers = MaryTtsServer
            .register(Providers::new(), &definition(Some("cmu-slt-hsmm"), None))
            .await
            .expect("builds without a server");

        assert_eq!(providers.tts().names().collect::<Vec<_>>(), ["house"]);
        let voice = providers.tts().get("house").expect("registered");
        assert_eq!(voice.descriptor().label, "House (MaryTTS)");
    }

    #[tokio::test]
    async fn a_voice_the_server_could_not_be_asked_for_fails_the_definition() {
        // The voice reaches a request parameter, so a definition carrying
        // something that is not a voice name refuses to become a provider while
        // an operator is still looking at the form.
        let error = MaryTtsServer
            .register(Providers::new(), &definition(Some("cmu slt&VOICE=other"), None))
            .await
            .expect_err("not a voice name");

        assert!(error.to_string().contains("voice"), "{error}");
    }

    #[test]
    fn a_marytts_definition_is_claimed_and_nothing_else_is() {
        assert!(MaryTtsServer.handles(&definition(None, None)));

        let mut other = definition(None, None);
        other.variant = ProviderDefinitionVariant::Tts {
            variant: TtsVariant::Wyoming {
                url: "tcp://piper:10200".to_owned(),
                voice: None,
                streaming: false,
            },
        };
        assert!(!MaryTtsServer.handles(&other), "the Wyoming factory's, not this one's");
    }

    #[tokio::test]
    async fn a_definition_this_factory_does_not_build_is_refused_rather_than_ignored() {
        // `handles` and `register` have to agree. If they ever disagree, the
        // definition is reported rather than silently producing no provider.
        let mut refused = definition(None, None);
        refused.variant = ProviderDefinitionVariant::Tts {
            variant: TtsVariant::Google { language: None, voice: None },
        };

        let error =
            MaryTtsServer.register(Providers::new(), &refused).await.expect_err("not ours");
        assert!(error.to_string().contains("house"), "{error}");
    }

    #[test]
    fn a_definition_that_names_no_locale_gets_the_crates_default() {
        // `/process` requires a locale whenever no voice is named, so an empty
        // one would make every unvoiced request fail.
        assert_eq!(
            config(&definition(None, None), "http://mary:59125", &None, &None).locale,
            "en_US"
        );
    }

    #[test]
    fn a_configured_locale_reaches_the_client() {
        let locale = Some("de".to_owned());
        assert_eq!(
            config(&definition(None, Some("de")), "http://mary:59125", &None, &locale).locale,
            "de"
        );
    }
}
