//! Utterance transforms that run in this process, from rules a definition
//! carries.

use conduit_core::Result;
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, TransformVariant,
};
use conduit_runtime::Providers;
use conduit_transform::Builtin;

use super::{unclaimed, ProviderFactory};

/// Rewriting an utterance with Conduit's own rules.
///
/// The one factory that reaches nothing outside the process: the rules are the
/// whole configuration.
pub struct BuiltinTransform;

#[async_trait::async_trait]
impl ProviderFactory for BuiltinTransform {
    fn name(&self) -> &'static str {
        "builtin-transform"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Transform { variant: TransformVariant::Builtin { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Transform {
            variant: TransformVariant::Builtin { rules },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };
        Ok(providers.with_transform(
            Builtin::new(&definition.id, rules.clone()).with_label(&definition.label),
        ))
    }
}
