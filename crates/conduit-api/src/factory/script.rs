//! Utterance transforms an operator wrote, compiled when the definition is
//! saved.
//!
//! The other factory that reaches nothing outside the process, and the only one
//! that *runs* something the operator supplied. Compilation happens here rather
//! than on the first utterance, so a script that cannot parse is refused while
//! the operator is still looking at it.

use conduit_core::Result;
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, ScriptEngine, TransformVariant,
};
use conduit_runtime::Providers;
use conduit_script::Script as ScriptTransform;

use super::{unclaimed, ProviderFactory};

/// Rewriting an utterance with a script.
///
/// Named for its role rather than for the type it builds, like
/// [`BedrockRuntime`]: `conduit_script::Script` is the provider, and this is the
/// thing that constructs one.
///
/// [`BedrockRuntime`]: super::BedrockRuntime
pub struct ScriptedTransform;

#[async_trait::async_trait]
impl ProviderFactory for ScriptedTransform {
    fn name(&self) -> &'static str {
        "script"
    }

    fn handles(&self, definition: &ProviderDefinition) -> bool {
        matches!(
            &definition.variant,
            ProviderDefinitionVariant::Transform { variant: TransformVariant::Script { .. } }
        )
    }

    async fn register(
        &self,
        providers: Providers,
        definition: &ProviderDefinition,
    ) -> Result<Providers> {
        let ProviderDefinitionVariant::Transform {
            variant: TransformVariant::Script { engine, source, timeout_ms },
        } = &definition.variant
        else {
            return Err(unclaimed(self.name(), definition));
        };
        // Matched rather than ignored: a second engine added to the enum should
        // fail to compile here rather than silently run its script on Rhai.
        match engine {
            ScriptEngine::Rhai => Ok(providers.with_transform(
                ScriptTransform::new(&definition.id, source.clone(), *timeout_ms)?
                    .with_label(&definition.label),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(source: &str, timeout_ms: u64) -> ProviderDefinition {
        ProviderDefinition {
            id: "shouting".to_owned(),
            label: "Shouting".to_owned(),
            variant: ProviderDefinitionVariant::Transform {
                variant: TransformVariant::Script {
                    engine: ScriptEngine::Rhai,
                    source: source.to_owned(),
                    timeout_ms,
                },
            },
            settings: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_stored_script_becomes_a_transform_under_its_own_id() {
        let providers = ScriptedTransform
            .register(Providers::default(), &definition("segment.to_upper()", 50))
            .await
            .expect("registers");

        assert_eq!(providers.transform().names().collect::<Vec<_>>(), ["shouting"]);
        let transform = providers.transform().get("shouting").expect("registered");
        assert_eq!(transform.descriptor().label, "Shouting");
    }

    #[tokio::test]
    async fn a_script_that_cannot_parse_fails_the_definition_not_the_turn() {
        // Compiling at registration is the whole reason this happens here: a
        // syntax error that surfaced on the first utterance would be a jammed
        // pipeline discovered by whoever spoke to it.
        let error = ScriptedTransform
            .register(Providers::default(), &definition("segment.to_upper(", 50))
            .await
            .expect_err("should be refused");
        assert!(error.to_string().contains("did not compile"), "{error}");
    }

    #[tokio::test]
    async fn a_deadline_the_engine_will_not_accept_fails_the_definition() {
        // The bound lives in `conduit-script`, and this is the check that it is
        // actually consulted rather than the definition being trusted.
        let error = ScriptedTransform
            .register(Providers::default(), &definition("segment", 60_000))
            .await
            .expect_err("should be refused");
        assert!(error.to_string().contains("deadline"), "{error}");
    }

    #[test]
    fn it_claims_scripts_and_leaves_the_builtin_rules_alone() {
        assert!(ScriptedTransform.handles(&definition("segment", 50)));

        let builtin = ProviderDefinition {
            id: "cleanup".to_owned(),
            label: "Cleanup".to_owned(),
            variant: ProviderDefinitionVariant::Transform {
                variant: TransformVariant::Builtin { rules: Vec::new() },
            },
            settings: Default::default(),
        };
        assert!(!ScriptedTransform.handles(&builtin));
    }
}
