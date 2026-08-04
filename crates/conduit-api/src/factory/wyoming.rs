//! The Wyoming vendor: recognition, synthesis, and wake word detection served
//! by a Wyoming protocol server.

use conduit_core::Result;
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, SttVariant, TtsVariant,
    DEFAULT_THRESHOLD_PERCENT,
};
use conduit_runtime::Providers;
use conduit_wyoming::stt::WyomingStt;
use conduit_wyoming::tts::WyomingTts;
use conduit_wyoming::wake::WyomingWake;

use super::{unclaimed, ProviderFactory};

/// Providers that live behind a Wyoming server.
///
/// A wake definition belongs here when it names an endpoint: which detector
/// the server runs is the server's business, and Conduit only speaks the
/// protocol to it.
pub struct Wyoming;

#[async_trait::async_trait]
impl ProviderFactory for Wyoming {
    fn name(&self) -> &'static str {
        "wyoming"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        match &definition.variant {
            ProviderDefinitionVariant::Stt { variant: SttVariant::Wyoming { .. } }
            | ProviderDefinitionVariant::Tts { variant: TtsVariant::Wyoming { .. } } => true,
            ProviderDefinitionVariant::Wake { variant } => variant.wyoming_url().is_some(),
            _ => false,
        }
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let id = &definition.id;
        let label = &definition.label;
        match &definition.variant {
            ProviderDefinitionVariant::Stt {
                variant: SttVariant::Wyoming { url, model, streaming },
            } => Ok(providers.with_stt(
                WyomingStt::new(id, url, model.clone(), *streaming)?.with_label(label),
            )),
            ProviderDefinitionVariant::Tts {
                variant: TtsVariant::Wyoming { url, voice, streaming },
            } => Ok(providers.with_tts(
                WyomingTts::new(id, url, voice.clone(), *streaming)?.with_label(label),
            )),
            ProviderDefinitionVariant::Wake { variant } => {
                let url =
                    variant.wyoming_url().ok_or_else(|| unclaimed(self.name(), definition))?;
                let threshold =
                    f32::from(variant.threshold_percent().unwrap_or(DEFAULT_THRESHOLD_PERCENT))
                        / 100.0;
                Ok(providers.with_wake(
                    WyomingWake::new(id, url, variant.phrases().to_vec(), threshold)?
                        .with_label(label),
                ))
            }
            _ => Err(unclaimed(self.name(), definition)),
        }
    }
}
