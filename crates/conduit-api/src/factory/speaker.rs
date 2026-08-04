//! Speaker identification vendors: a Diarization_Server instance, and any
//! service speaking the Conduit speaker HTTP contract.

use conduit_core::Result;
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, SpeakerIdVariant,
};
use conduit_runtime::Providers;
use conduit_speaker::diarization_server::DiarizationServerSpeakerId;
use conduit_speaker::HttpSpeakerId;

use super::{secret_value, unclaimed, ProviderFactory};

/// Voices identified by a Diarization_Server instance.
///
/// Its own factory rather than a branch of [`HttpSpeaker`] because the two
/// speak different dialects — raw samples and query parameters against a
/// container, against Conduit's own contract — which is why they are separate
/// definition variants too.
pub struct DiarizationServer;

#[async_trait::async_trait]
impl ProviderFactory for DiarizationServer {
    fn name(&self) -> &'static str {
        "diarization-server"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::SpeakerId {
                variant: SpeakerIdVariant::DiarizationServer { .. }
            }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::DiarizationServer { base_url, threshold_percent },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };
        Ok(providers.with_speaker(
            DiarizationServerSpeakerId::new(
                &definition.id,
                base_url,
                fraction(*threshold_percent),
            )?
            .with_label(&definition.label),
        ))
    }
}

/// Voices identified by a service speaking the Conduit speaker HTTP contract.
pub struct HttpSpeaker;

#[async_trait::async_trait]
impl ProviderFactory for HttpSpeaker {
    fn name(&self) -> &'static str {
        "http-speaker"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::SpeakerId { variant: SpeakerIdVariant::Http { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::Http { base_url, api_key, threshold_percent, .. },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };
        Ok(providers.with_speaker(HttpSpeakerId::new(
            &definition.id,
            base_url,
            secret_value(api_key),
            fraction(*threshold_percent),
        )?))
    }
}

/// The percentage a definition stores, as the fraction the providers take.
fn fraction(threshold_percent: u8) -> f32 {
    f32::from(threshold_percent) / 100.0
}
