//! Transformation rule types and conditions

use serde::{Deserialize, Serialize};

/// A single Forma transformation rule
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormaRule {
    /// Unique identifier for this rule
    pub id: String,
    
    /// Human-readable name for this rule
    pub name: String,
    
    /// Description of what this rule does
    pub description: String,
    
    /// Type of transformation this rule performs
    pub rule_type: RuleType,
    
    /// When this rule should be applied
    pub condition: RuleCondition,
    
    /// The action to take when the condition is met
    pub action: RuleAction,
    
    /// Whether this rule is enabled
    pub enabled: bool,
    
    /// Priority for ordering rules (higher = applied first)
    pub priority: i32,
}

/// Types of transformation rules
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleType {
    /// Replace matching text with new text
    Replace,
    
    /// Remove matching text
    Remove,
    
    /// Transform matching text (e.g., case conversion)
    Transform,
    
    /// Insert text before/after matches
    Insert,
    
    /// Custom script-based transformation
    Script,
}

/// Conditions for when a rule should be applied
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleCondition {
    /// Apply to all text
    Always,
    
    /// Apply only if text matches a pattern
    MatchesPattern { pattern: String, flags: String },
    
    /// Apply only if text contains a substring
    Contains { substring: String },
    
    /// Apply only if text starts with a substring
    StartsWith { prefix: String },
    
    /// Apply only if text ends with a substring
    EndsWith { suffix: String },
    
    /// Apply only if custom condition evaluates to true
    Custom { condition: String },
}

/// Actions to take when a rule's condition is met
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// Replace matching text with new text
    Replace { pattern: String, replacement: String, flags: String },
    
    /// Remove matching text
    Remove { pattern: String, flags: String },
    
    /// Convert text case
    ConvertCase { case: CaseConversion },
    
    /// Insert text before matches
    InsertBefore { pattern: String, text: String, flags: String },
    
    /// Insert text after matches
    InsertAfter { pattern: String, text: String, flags: String },
    
    /// Apply custom transformation script
    CustomScript { script: String },
}

/// Case conversion options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaseConversion {
    Upper,
    Lower,
    Title,
    Sentence,
}

impl FormaRule {
    /// Create a new rule with the given ID and name
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            rule_type: RuleType::Replace,
            condition: RuleCondition::Always,
            action: RuleAction::Replace {
                pattern: String::new(),
                replacement: String::new(),
                flags: String::new(),
            },
            enabled: true,
            priority: 0,
        }
    }
    
    /// Set the rule description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
    
    /// Set the rule type
    pub fn with_type(mut self, rule_type: RuleType) -> Self {
        self.rule_type = rule_type;
        self
    }
    
    /// Set the rule condition
    pub fn with_condition(mut self, condition: RuleCondition) -> Self {
        self.condition = condition;
        self
    }
    
    /// Set the rule action
    pub fn with_action(mut self, action: RuleAction) -> Self {
        self.action = action;
        self
    }
    
    /// Set whether this rule is enabled
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    
    /// Set the rule priority
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }
    
    /// Validate this rule's configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("Rule ID cannot be empty".to_string());
        }
        
        if self.name.is_empty() {
            return Err("Rule name cannot be empty".to_string());
        }
        
        match &self.action {
            RuleAction::Replace { pattern, .. } => {
                if pattern.is_empty() {
                    return Err("Replace action requires a pattern".to_string());
                }
            }
            RuleAction::Remove { pattern, .. } => {
                if pattern.is_empty() {
                    return Err("Remove action requires a pattern".to_string());
                }
            }
            RuleAction::InsertBefore { pattern, text, .. } => {
                if pattern.is_empty() {
                    return Err("InsertBefore action requires a pattern".to_string());
                }
                if text.is_empty() {
                    return Err("InsertBefore action requires text to insert".to_string());
                }
            }
            RuleAction::InsertAfter { pattern, text, .. } => {
                if pattern.is_empty() {
                    return Err("InsertAfter action requires a pattern".to_string());
                }
                if text.is_empty() {
                    return Err("InsertAfter action requires text to insert".to_string());
                }
            }
            RuleAction::CustomScript { script } => {
                if script.is_empty() {
                    return Err("CustomScript action requires a script".to_string());
                }
            }
            RuleAction::ConvertCase { .. } => {}
        }
        
        Ok(())
    }
}