//! Wake word detectors that do not live behind a Wyoming server: models
//! scored in this process, and satellites that wake themselves.
//!
//! Where a wake definition detects is the shape of the definition, so it is
//! also what decides which factory builds it — a Wyoming endpoint goes to
//! [`Wyoming`](super::Wyoming), models on disk come here, and a satellite that
//! already decided still gets a detector here: one that scores nothing, so
//! that a pipeline naming the stage resolves and the activation reaches the
//! event stream.

use std::path::PathBuf;

use conduit_core::{Error, Result};
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, WakeEngine, WakeVariant,
    DEFAULT_THRESHOLD_PERCENT,
};
use conduit_provider::wake::DeviceWake as DeviceWakeProvider;
use conduit_runtime::Providers;
use conduit_wake::OpenWakeWord as OpenWakeWordProvider;

use super::{unclaimed, ProviderFactory};

/// Phrase models loaded from disk and scored in the Conduit process.
///
/// Claims every definition that names a local directory, including engines
/// this process cannot score, so that such a definition is told exactly what
/// is wrong with it rather than reported as a shape nothing recognizes.
pub struct OpenWakeWord;

#[async_trait::async_trait]
impl ProviderFactory for OpenWakeWord {
    fn name(&self) -> &'static str {
        "openwakeword"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        wake_variant(definition).is_some_and(|variant| variant.local_models_dir().is_some())
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let variant =
            wake_variant(definition).ok_or_else(|| unclaimed(self.name(), definition))?;
        let Some(models_dir) = variant.local_models_dir() else {
            return Err(unclaimed(self.name(), definition));
        };
        if variant.engine() != WakeEngine::OpenWakeWord {
            return Err(Error::Config(crate::providers::local_wake_unavailable(
                variant.engine().name(),
            )));
        }
        // A definition that named no directory means the conventional one under
        // the data directory, which is the volume the compose file mounts.
        let directory = match models_dir {
            Some(named) => PathBuf::from(named),
            None => crate::config::wake_models_dir_from_env()?,
        };
        Ok(providers.with_wake(
            OpenWakeWordProvider::load(
                &definition.id,
                directory,
                variant.phrases().to_vec(),
                threshold(variant),
            )?
            .with_label(&definition.label),
        ))
    }
}

/// A satellite that runs the detector itself.
///
/// There is nothing to score here — the device already activated — so what
/// gets registered is a detector that reports the activation and nothing else.
pub struct DeviceWake;

#[async_trait::async_trait]
impl ProviderFactory for DeviceWake {
    fn name(&self) -> &'static str {
        "device-wake"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        wake_variant(definition).is_some_and(|variant| {
            variant.wyoming_url().is_none() && variant.local_models_dir().is_none()
        })
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let variant =
            wake_variant(definition).ok_or_else(|| unclaimed(self.name(), definition))?;
        Ok(providers.with_wake(
            DeviceWakeProvider::new(&definition.id, variant.phrases().to_vec())
                .with_label(&definition.label),
        ))
    }
}

/// The wake settings a definition carries, if it is a wake definition at all.
fn wake_variant(definition: &ProviderDefinition) -> Option<&WakeVariant> {
    match &definition.variant {
        ProviderDefinitionVariant::Wake { variant } => Some(variant),
        _ => None,
    }
}

/// The confidence a detection must reach, as the fraction the providers take.
fn threshold(variant: &WakeVariant) -> f32 {
    f32::from(variant.threshold_percent().unwrap_or(DEFAULT_THRESHOLD_PERCENT)) / 100.0
}
