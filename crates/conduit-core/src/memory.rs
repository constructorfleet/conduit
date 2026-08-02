//! The memory vocabulary shared by the pipeline graph and memory providers.
//!
//! A graph has to say how long what a memory node stores should live, and so
//! does the provider that stores it. Those are the same three words, and
//! `conduit-provider` depends on this crate rather than the other way round, so
//! the vocabulary lives here and `conduit_provider::memory` re-exports it.
//! Spelling it twice would leave two enums that must agree and no compiler
//! obliging them to.

use serde::{Deserialize, Serialize};

/// How long a record should be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Lives only as long as the conversation.
    Conversation,
    /// Persists across conversations for one speaker.
    Speaker,
    /// Persists for everyone.
    Global,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_written_in_snake_case() {
        // Graphs and stored records both spell a scope on the wire, and an
        // operator editing either by hand reads the same three words.
        for (scope, spelling) in [
            (Scope::Conversation, "\"conversation\""),
            (Scope::Speaker, "\"speaker\""),
            (Scope::Global, "\"global\""),
        ] {
            assert_eq!(serde_json::to_string(&scope).expect("serialize"), spelling);
        }
    }
}
