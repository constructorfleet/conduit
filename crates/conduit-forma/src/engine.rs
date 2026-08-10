//! Transformation engine that applies Forma rules to text

use crate::rule::{CaseConversion, FormaRule, RuleAction, RuleCondition};
use crate::FormaError;
use regex::Regex;

/// Engine for applying Forma transformation rules
pub struct Engine {
    /// Compiled regex cache for performance
    regex_cache: std::sync::Mutex<std::collections::HashMap<String, Regex>>,
}

impl Engine {
    /// Create a new transformation engine
    pub fn new() -> Self {
        Self { regex_cache: std::sync::Mutex::new(std::collections::HashMap::new()) }
    }

    /// Apply a single rule to text
    pub fn apply_rule(&self, text: &str, rule: &FormaRule) -> Result<String, FormaError> {
        if !rule.enabled {
            return Ok(text.to_string());
        }

        // Check if the condition is met
        if !self.check_condition(text, &rule.condition)? {
            return Ok(text.to_string());
        }

        // Apply the transformation
        self.apply_action(text, &rule.action)
    }

    /// Apply multiple rules in sequence
    pub fn apply_rules(&self, text: &str, rules: &[FormaRule]) -> Result<String, FormaError> {
        let mut result = text.to_string();

        // Sort rules by priority (higher first)
        let mut sorted_rules = rules.to_vec();
        sorted_rules.sort_by_key(|rule| std::cmp::Reverse(rule.priority));

        for rule in &sorted_rules {
            result = self.apply_rule(&result, rule)?;
        }

        Ok(result)
    }

    /// Check if a rule's condition is met for the given text
    fn check_condition(
        &self,
        text: &str,
        condition: &RuleCondition,
    ) -> Result<bool, FormaError> {
        match condition {
            RuleCondition::Always => Ok(true),

            RuleCondition::MatchesPattern { pattern, flags } => {
                let regex = self.compile_regex(pattern, flags)?;
                Ok(regex.is_match(text))
            }

            RuleCondition::Contains { substring } => Ok(text.contains(substring)),

            RuleCondition::StartsWith { prefix } => Ok(text.starts_with(prefix)),

            RuleCondition::EndsWith { suffix } => Ok(text.ends_with(suffix)),

            RuleCondition::Custom { condition: _ } => {
                // TODO: Implement custom condition evaluation
                // For now, we'll return true to avoid blocking users
                Ok(true)
            }
        }
    }

    /// Apply a rule's action to text
    fn apply_action(&self, text: &str, action: &RuleAction) -> Result<String, FormaError> {
        match action {
            RuleAction::Replace { pattern, replacement, flags } => {
                let regex = self.compile_regex(pattern, flags)?;
                Ok(regex.replace_all(text, replacement).to_string())
            }

            RuleAction::Remove { pattern, flags } => {
                let regex = self.compile_regex(pattern, flags)?;
                Ok(regex.replace_all(text, "").to_string())
            }

            RuleAction::ConvertCase { case } => match case {
                CaseConversion::Upper => Ok(text.to_uppercase()),
                CaseConversion::Lower => Ok(text.to_lowercase()),
                CaseConversion::Title => {
                    let result = text
                        .chars()
                        .enumerate()
                        .map(|(i, c)| {
                            if i == 0
                                || text.chars().nth(i - 1).is_some_and(char::is_whitespace)
                            {
                                c.to_uppercase().collect::<String>()
                            } else {
                                c.to_lowercase().collect::<String>()
                            }
                        })
                        .collect::<String>();
                    Ok(result)
                }
                CaseConversion::Sentence => {
                    let result = text
                        .chars()
                        .enumerate()
                        .map(|(i, c)| {
                            if i == 0 || text.chars().nth(i - 1).is_some_and(|prev| prev == '.')
                            {
                                c.to_uppercase().collect::<String>()
                            } else {
                                c.to_lowercase().collect::<String>()
                            }
                        })
                        .collect::<String>();
                    Ok(result)
                }
            },

            RuleAction::InsertBefore { pattern, text: insert_text, flags } => {
                let regex = self.compile_regex(pattern, flags)?;
                Ok(regex.replace_all(text, format!("{}$0", insert_text)).to_string())
            }

            RuleAction::InsertAfter { pattern, text: insert_text, flags } => {
                let regex = self.compile_regex(pattern, flags)?;
                Ok(regex.replace_all(text, format!("$0{}", insert_text)).to_string())
            }

            RuleAction::CustomScript { script: _ } => {
                // TODO: Implement custom script execution
                // For now, return text unchanged
                Ok(text.to_string())
            }
        }
    }

    /// Compile a regex pattern with optional flags
    fn compile_regex(&self, pattern: &str, flags: &str) -> Result<Regex, FormaError> {
        let cache_key = format!("{}|{}", pattern, flags);

        {
            let cache = self
                .regex_cache
                .lock()
                .map_err(|e| FormaError::Execution(format!("Lock error: {e}")))?;

            if let Some(regex) = cache.get(&cache_key) {
                return Ok(regex.clone());
            }
        }

        let mut regex_builder = RegexBuilder::new(pattern);

        for flag in flags.chars() {
            match flag {
                'i' => {
                    regex_builder.case_insensitive(true);
                }
                'm' => {
                    regex_builder.multi_line(true);
                }
                's' => {
                    regex_builder.dot_matches_new_line(true);
                }
                'x' => {
                    regex_builder.ignore_whitespace(true);
                }
                'U' => {
                    regex_builder.unicode(true);
                }
                _ => {}
            }
        }

        let regex = regex_builder
            .build()
            .map_err(|e| FormaError::Validation(format!("Invalid regex pattern: {e}")))?;

        let mut cache = self
            .regex_cache
            .lock()
            .map_err(|e| FormaError::Execution(format!("Lock error: {e}")))?;
        cache.insert(cache_key, regex.clone());

        Ok(regex)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

// Re-export RegexBuilder
use regex::RegexBuilder;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{RuleAction, RuleCondition, RuleType};

    fn create_replace_rule(pattern: &str, replacement: &str) -> FormaRule {
        FormaRule::new("test", "Test Rule")
            .with_description("Test replace rule")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Replace {
                pattern: pattern.to_string(),
                replacement: replacement.to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(0)
    }

    #[test]
    fn applies_simple_replacement() {
        let engine = Engine::new();
        let rule = create_replace_rule("hello", "hi");

        let result = engine.apply_rule("hello world", &rule).unwrap();
        assert_eq!(result, "hi world");
    }

    #[test]
    fn removes_matching_text() {
        let engine = Engine::new();
        let rule = FormaRule::new("test", "Test Rule")
            .with_description("Test remove rule")
            .with_type(RuleType::Remove)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Remove {
                pattern: "bad".to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(0);

        let result = engine.apply_rule("this is bad text", &rule).unwrap();
        assert_eq!(result, "this is  text");
    }

    #[test]
    fn converts_case() {
        let engine = Engine::new();
        let rule = FormaRule::new("test", "Test Rule")
            .with_description("Test case rule")
            .with_type(RuleType::Transform)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::ConvertCase { case: CaseConversion::Upper })
            .with_enabled(true)
            .with_priority(0);

        let result = engine.apply_rule("hello world", &rule).unwrap();
        assert_eq!(result, "HELLO WORLD");
    }

    #[test]
    fn applies_conditional_rule() {
        let engine = Engine::new();
        let rule = FormaRule::new("test", "Test Rule")
            .with_description("Test conditional rule")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Contains { substring: "special".to_string() })
            .with_action(RuleAction::Replace {
                pattern: "hello".to_string(),
                replacement: "hi".to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(0);

        let result1 = engine.apply_rule("special hello world", &rule).unwrap();
        assert_eq!(result1, "special hi world");

        let result2 = engine.apply_rule("normal hello world", &rule).unwrap();
        assert_eq!(result2, "normal hello world");
    }

    #[test]
    fn applies_multiple_rules() {
        let engine = Engine::new();

        let rule1 = create_replace_rule("hello", "hi");
        let rule2 = FormaRule::new("test2", "Test Rule 2")
            .with_description("Test case rule")
            .with_type(RuleType::Transform)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::ConvertCase { case: CaseConversion::Upper })
            .with_enabled(true)
            .with_priority(0);

        let rules = vec![rule1, rule2];
        let result = engine.apply_rules("hello world", &rules).unwrap();
        assert_eq!(result, "HI WORLD");
    }

    #[test]
    fn respects_rule_priority() {
        let engine = Engine::new();

        let rule1 = FormaRule::new("test1", "Test Rule 1")
            .with_description("First replace")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Replace {
                pattern: "hello".to_string(),
                replacement: "hi".to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(10);

        let rule2 = FormaRule::new("test2", "Test Rule 2")
            .with_description("Second replace")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Replace {
                pattern: "hi".to_string(),
                replacement: "hey".to_string(),
                flags: String::new(),
            })
            .with_enabled(true)
            .with_priority(5);

        let rules = vec![rule1, rule2];
        let result = engine.apply_rules("hello world", &rules).unwrap();
        // Higher priority (10) runs first, so "hello" -> "hi", then "hi" -> "hey"
        assert_eq!(result, "hey world");
    }

    #[test]
    fn skips_disabled_rules() {
        let engine = Engine::new();
        let rule = FormaRule::new("test", "Test Rule")
            .with_description("Disabled rule")
            .with_type(RuleType::Replace)
            .with_condition(RuleCondition::Always)
            .with_action(RuleAction::Replace {
                pattern: "hello".to_string(),
                replacement: "hi".to_string(),
                flags: String::new(),
            })
            .with_enabled(false)
            .with_priority(0);

        let result = engine.apply_rule("hello world", &rule).unwrap();
        assert_eq!(result, "hello world");
    }
}
