//! Forma - Custom transformation rule engine for Conduit
//!
//! Forma allows users to define transformation rules for utterances prior to
//! output/TTS. It extends beyond the built-in transforms by providing a flexible,
//! user-configurable rule system that can be applied as a transform provider.
//!
//! ## Features
//!
//! - Custom transformation rules via regex patterns
//! - Text replacement and modification
//! - Conditional transformations based on content
//! - Chainable rule sets for complex transformations
//! - Integration with Conduit's transform pipeline

pub mod engine;
pub mod rule;
pub mod storage;
pub mod provider;

pub use engine::Engine;
pub use rule::{FormaRule, RuleType, RuleCondition, RuleAction, CaseConversion};
pub use provider::FormaProvider;
pub use storage::{FormaStore, MemoryStore, RuleSet};

/// Error types for Forma operations
#[derive(Debug, thiserror::Error)]
pub enum FormaError {
    #[error("Rule validation failed: {0}")]
    Validation(String),
    
    #[error("Rule execution failed: {0}")]
    Execution(String),
    
    #[error("Storage error: {0}")]
    Storage(String),
    
    #[error("Invalid rule ID: {0}")]
    InvalidRuleId(String),
    
    #[error("Rule set not found: {0}")]
    RuleSetNotFound(String),
}