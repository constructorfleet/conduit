//! Forma provider implementation for Conduit transform integration

use crate::engine::Engine;
use crate::storage::{FormaStore, MemoryStore};
use crate::FormaError;
use async_trait::async_trait;
use conduit_core::Result;
use conduit_provider::transform::UtteranceTransform;
use conduit_provider::{Capability, Descriptor, Provider};
use std::sync::Arc;

/// Forma provider that applies custom transformation rules
#[derive(Clone)]
pub struct FormaProvider {
    /// Provider descriptor
    descriptor: Descriptor,

    /// Transformation engine
    engine: Arc<Engine>,

    /// Storage for rules and rule sets
    store: Arc<dyn FormaStore>,

    /// ID of the rule set to use
    rule_set_id: Option<String>,
}

impl FormaProvider {
    /// Create a new Forma provider
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            descriptor: Descriptor::new(name, Capability::Transform),
            engine: Arc::new(Engine::new()),
            store: Arc::new(MemoryStore::new()),
            rule_set_id: None,
        }
    }

    /// Create a Forma provider with a custom storage backend
    pub fn with_storage(name: impl Into<String>, store: Arc<dyn FormaStore>) -> Self {
        Self {
            descriptor: Descriptor::new(name, Capability::Transform),
            engine: Arc::new(Engine::new()),
            store,
            rule_set_id: None,
        }
    }

    /// Set the rule set to use for transformations
    pub fn with_rule_set(mut self, rule_set_id: impl Into<String>) -> Self {
        self.rule_set_id = Some(rule_set_id.into());
        self
    }

    /// Set the human-readable label
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.with_label(label);
        self
    }

    /// Get the storage backend
    pub fn store(&self) -> &Arc<dyn FormaStore> {
        &self.store
    }

    /// Apply transformation using the configured rule set
    async fn transform_with_rules(&self, segment: &str) -> Result<String> {
        let rules = if let Some(rule_set_id) = &self.rule_set_id {
            self.store
                .get_rule_set(rule_set_id)
                .await
                .map_err(|e: FormaError| {
                    conduit_core::Error::Config(format!("Forma storage error: {e}"))
                })?
                .map(|set| set.rules)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        self.engine.apply_rules(segment, &rules).map_err(|e| {
            conduit_core::Error::Config(format!("Forma transformation failed: {e}"))
        })
    }
}

#[async_trait]
impl Provider for FormaProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
}

#[async_trait]
impl UtteranceTransform for FormaProvider {
    async fn transform(&self, segment: &str) -> Result<String> {
        self.transform_with_rules(segment).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{RuleAction, RuleCondition, RuleType};

    fn create_test_rule_set() -> crate::storage::RuleSet {
        let rule = crate::rule::FormaRule::new("test-rule", "Test Rule")
            .with_description("Replace hello with hi")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Replace {
                pattern: "hello".to_string(),
                replacement: "hi".to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(0);

        crate::storage::RuleSet {
            id: "test-set".to_string(),
            name: "Test Set".to_string(),
            description: "Test rule set".to_string(),
            rules: vec![rule],
        }
    }

    #[tokio::test]
    async fn transforms_text_using_rules() {
        let store = Arc::new(MemoryStore::new());
        let rule_set = create_test_rule_set();

        store.create_rule_set(rule_set).await.unwrap();

        let provider =
            FormaProvider::with_storage("test-forma", store).with_rule_set("test-set");

        let result = provider.transform("hello world").await.unwrap();
        assert_eq!(result, "hi world");
    }

    #[tokio::test]
    async fn returns_unchanged_when_no_rules_configured() {
        let provider = FormaProvider::new("test-forma");

        let result = provider.transform("hello world").await.unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn handles_missing_rule_set() {
        let store = Arc::new(MemoryStore::new());
        let provider =
            FormaProvider::with_storage("test-forma", store).with_rule_set("nonexistent-set");

        let result = provider.transform("hello world").await.unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn reports_provider_name() {
        let provider = FormaProvider::new("my-forma");
        assert_eq!(Provider::name(&provider), "my-forma");
    }

    #[tokio::test]
    async fn applies_multiple_rules_in_priority_order() {
        let store = Arc::new(MemoryStore::new());

        let rule1 = crate::rule::FormaRule::new("rule1", "Rule 1")
            .with_description("First transformation")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Replace {
                pattern: "hello".to_string(),
                replacement: "hi".to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(10);

        let rule2 = crate::rule::FormaRule::new("rule2", "Rule 2")
            .with_description("Second transformation")
            .with_type(RuleType::Transform)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::ConvertCase { case: crate::rule::CaseConversion::Upper })
            .with_enabled(true)
            .with_priority(5);

        let rule_set = crate::storage::RuleSet {
            id: "test-set".to_string(),
            name: "Test Set".to_string(),
            description: "Test rule set".to_string(),
            rules: vec![rule1, rule2],
        };

        store.create_rule_set(rule_set).await.unwrap();

        let provider =
            FormaProvider::with_storage("test-forma", store).with_rule_set("test-set");

        let result = provider.transform("hello world").await.unwrap();
        // Higher priority (10) runs first: "hello" -> "hi", then upper: "hi world" -> "HI WORLD"
        assert_eq!(result, "HI WORLD");
    }
}
