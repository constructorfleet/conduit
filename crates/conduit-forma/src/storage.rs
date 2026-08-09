//! In-memory storage for Forma rules and rule sets

use crate::rule::FormaRule;
use crate::FormaError;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// A rule set is a named collection of transformation rules
#[derive(Debug, Clone)]
pub struct RuleSet {
    /// Unique identifier for this rule set
    pub id: String,
    
    /// Human-readable name
    pub name: String,
    
    /// Description of this rule set's purpose
    pub description: String,
    
    /// The rules in this set, ordered by priority
    pub rules: Vec<FormaRule>,
}

/// Storage backend for Forma rules and rule sets
#[async_trait::async_trait]
pub trait FormaStore: Send + Sync {
    /// Create a new rule set
    async fn create_rule_set(&self, rule_set: RuleSet) -> Result<(), FormaError>;
    
    /// Get a rule set by ID
    async fn get_rule_set(&self, id: &str) -> Result<Option<RuleSet>, FormaError>;
    
    /// List all rule sets
    async fn list_rule_sets(&self) -> Result<Vec<RuleSet>, FormaError>;
    
    /// Update a rule set
    async fn update_rule_set(&self, rule_set: RuleSet) -> Result<(), FormaError>;
    
    /// Delete a rule set
    async fn delete_rule_set(&self, id: &str) -> Result<(), FormaError>;
    
    /// Add a rule to a rule set
    async fn add_rule(&self, rule_set_id: &str, rule: FormaRule) -> Result<(), FormaError>;
    
    /// Update a rule in a rule set
    async fn update_rule(&self, rule_set_id: &str, rule: FormaRule) -> Result<(), FormaError>;
    
    /// Delete a rule from a rule set
    async fn delete_rule(&self, rule_set_id: &str, rule_id: &str) -> Result<(), FormaError>;
}

/// In-memory implementation of FormaStore
pub struct MemoryStore {
    rule_sets: Arc<RwLock<HashMap<String, RuleSet>>>,
}

impl MemoryStore {
    /// Create a new in-memory store
    pub fn new() -> Self {
        Self {
            rule_sets: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FormaStore for MemoryStore {
    async fn create_rule_set(&self, rule_set: RuleSet) -> Result<(), FormaError> {
        let mut store = self.rule_sets.write()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        if store.contains_key(&rule_set.id) {
            return Err(FormaError::Storage(format!("Rule set {} already exists", rule_set.id)));
        }
        
        store.insert(rule_set.id.clone(), rule_set);
        Ok(())
    }
    
    async fn get_rule_set(&self, id: &str) -> Result<Option<RuleSet>, FormaError> {
        let store = self.rule_sets.read()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        Ok(store.get(id).cloned())
    }
    
    async fn list_rule_sets(&self) -> Result<Vec<RuleSet>, FormaError> {
        let store = self.rule_sets.read()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        Ok(store.values().cloned().collect())
    }
    
    async fn update_rule_set(&self, rule_set: RuleSet) -> Result<(), FormaError> {
        let mut store = self.rule_sets.write()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        if !store.contains_key(&rule_set.id) {
            return Err(FormaError::RuleSetNotFound(rule_set.id));
        }
        
        store.insert(rule_set.id.clone(), rule_set);
        Ok(())
    }
    
    async fn delete_rule_set(&self, id: &str) -> Result<(), FormaError> {
        let mut store = self.rule_sets.write()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        if store.remove(id).is_none() {
            return Err(FormaError::RuleSetNotFound(id.to_string()));
        }
        
        Ok(())
    }
    
    async fn add_rule(&self, rule_set_id: &str, rule: FormaRule) -> Result<(), FormaError> {
        let mut store = self.rule_sets.write()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        let rule_set = store.get_mut(rule_set_id)
            .ok_or_else(|| FormaError::RuleSetNotFound(rule_set_id.to_string()))?;
        
        rule_set.rules.push(rule);
        rule_set.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(())
    }
    
    async fn update_rule(&self, rule_set_id: &str, rule: FormaRule) -> Result<(), FormaError> {
        let mut store = self.rule_sets.write()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        let rule_set = store.get_mut(rule_set_id)
            .ok_or_else(|| FormaError::RuleSetNotFound(rule_set_id.to_string()))?;
        
        let pos = rule_set.rules.iter()
            .position(|r| r.id == rule.id)
            .ok_or_else(|| FormaError::InvalidRuleId(rule.id.clone()))?;
        
        rule_set.rules[pos] = rule;
        rule_set.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(())
    }
    
    async fn delete_rule(&self, rule_set_id: &str, rule_id: &str) -> Result<(), FormaError> {
        let mut store = self.rule_sets.write()
            .map_err(|e| FormaError::Storage(format!("Lock error: {e}")))?;
        
        let rule_set = store.get_mut(rule_set_id)
            .ok_or_else(|| FormaError::RuleSetNotFound(rule_set_id.to_string()))?;
        
        let initial_len = rule_set.rules.len();
        rule_set.rules.retain(|r| r.id != rule_id);
        
        if rule_set.rules.len() == initial_len {
            return Err(FormaError::InvalidRuleId(rule_id.to_string()));
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{RuleAction, RuleCondition, RuleType};
    
    fn create_test_rule(id: &str, name: &str) -> FormaRule {
        FormaRule::new(id, name)
            .with_description("Test rule")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Replace {
                pattern: "test".to_string(),
                replacement: "demo".to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(0)
    }
    
    fn create_test_rule_set(id: &str, name: &str) -> RuleSet {
        RuleSet {
            id: id.to_string(),
            name: name.to_string(),
            description: "Test rule set".to_string(),
            rules: vec![create_test_rule("rule1", "Rule 1")],
        }
    }
    
    #[tokio::test]
    async fn can_create_and_retrieve_rule_set() {
        let store = MemoryStore::new();
        let rule_set = create_test_rule_set("test-set", "Test Set");
        
        store.create_rule_set(rule_set.clone()).await.unwrap();
        
        let retrieved = store.get_rule_set("test-set").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "Test Set");
    }
    
    #[tokio::test]
    async fn cannot_duplicate_rule_set() {
        let store = MemoryStore::new();
        let rule_set = create_test_rule_set("test-set", "Test Set");
        
        store.create_rule_set(rule_set.clone()).await.unwrap();
        let result = store.create_rule_set(rule_set).await;
        
        assert!(result.is_err());
    }
    
    #[tokio::test]
    async fn can_list_rule_sets() {
        let store = MemoryStore::new();
        
        store.create_rule_set(create_test_rule_set("set1", "Set 1")).await.unwrap();
        store.create_rule_set(create_test_rule_set("set2", "Set 2")).await.unwrap();
        
        let sets = store.list_rule_sets().await.unwrap();
        assert_eq!(sets.len(), 2);
    }
    
    #[tokio::test]
    async fn can_add_rule_to_set() {
        let store = MemoryStore::new();
        store.create_rule_set(create_test_rule_set("test-set", "Test Set")).await.unwrap();
        
        let new_rule = create_test_rule("rule2", "Rule 2");
        store.add_rule("test-set", new_rule).await.unwrap();
        
        let rule_set = store.get_rule_set("test-set").await.unwrap().unwrap();
        assert_eq!(rule_set.rules.len(), 2);
    }
    
    #[tokio::test]
    async fn can_delete_rule_from_set() {
        let store = MemoryStore::new();
        store.create_rule_set(create_test_rule_set("test-set", "Test Set")).await.unwrap();
        
        store.delete_rule("test-set", "rule1").await.unwrap();
        
        let rule_set = store.get_rule_set("test-set").await.unwrap().unwrap();
        assert_eq!(rule_set.rules.len(), 0);
    }
}