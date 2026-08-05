//! Voice activity detection, scored in this process from a model on disk.
//!
//! There is one factory here and no Wyoming counterpart, because there is no
//! service: Silero is a file under two megabytes, so what an operator
//! configures is where the file is and what the detector decides, and both are
//! read straight off the definition.

use std::path::PathBuf;

use conduit_core::Result;
use conduit_provider::storage::{ProviderDefinition, ProviderDefinitionVariant, VadVariant};
use conduit_runtime::Providers;
use conduit_vad::SileroVad;

use super::{unclaimed, ProviderFactory};

/// The Silero model, loaded from disk and scored in the Conduit process.
pub struct Silero;

#[async_trait::async_trait]
impl ProviderFactory for Silero {
    fn name(&self) -> &'static str {
        "silero"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        vad_variant(definition).is_some()
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let variant =
            vad_variant(definition).ok_or_else(|| unclaimed(self.name(), definition))?;
        // Not a `let` binding: `VadVariant` is `#[non_exhaustive]`, so a variant
        // added later must fail here loudly rather than be silently skipped by a
        // factory that claimed it.
        let VadVariant::Silero { model_path, threshold_percent, silence_ms } = variant else {
            return Err(unclaimed(self.name(), definition));
        };
        // A definition that named no path means the conventional file under the
        // data directory, which is the volume the compose file mounts — the same
        // convention the local wake runtime follows.
        let path = match model_path {
            Some(named) => PathBuf::from(named),
            None => crate::config::vad_model_path_from_env()?,
        };
        Ok(providers.with_vad(
            SileroVad::load(
                &definition.id,
                path,
                f32::from(*threshold_percent) / 100.0,
                *silence_ms,
            )?
            .with_label(&definition.label),
        ))
    }
}

/// The detection settings a definition carries, if it is a detector at all.
fn vad_variant(definition: &ProviderDefinition) -> Option<&VadVariant> {
    match &definition.variant {
        ProviderDefinitionVariant::Vad { variant } => Some(variant),
        _ => None,
    }
}
