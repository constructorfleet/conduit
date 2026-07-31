//! Memory provider interface.
//!
//! Memory is deliberately narrow: store a record, search records. Whether
//! that is Postgres with pgvector, SQLite with a brute-force scan, or an
//! in-process map is the backend's business.

use conduit_core::id::{ConversationId, SpeakerId};
use conduit_core::Result;
use serde::{Deserialize, Serialize};

use crate::Provider;

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

/// Something worth remembering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The text to store and later match against.
    pub content: String,
    /// How long to keep it.
    pub scope: Scope,
    /// Conversation this belongs to, required for [`Scope::Conversation`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationId>,
    /// Speaker this belongs to, required for [`Scope::Speaker`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<SpeakerId>,
    /// Caller-defined metadata, returned unchanged on retrieval.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub metadata: serde_json::Value,
}

/// What to retrieve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// Text to match against, semantically or otherwise.
    pub text: String,
    /// Maximum records to return.
    pub limit: usize,
    /// Restrict to one scope, or search all when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    /// Restrict to one conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationId>,
    /// Restrict to one speaker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<SpeakerId>,
}

impl Query {
    /// An unrestricted query for the `limit` best matches.
    #[must_use]
    pub fn new(text: impl Into<String>, limit: usize) -> Self {
        Self { text: text.into(), limit, scope: None, conversation: None, speaker: None }
    }
}

/// A retrieved record and how well it matched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    /// The stored record.
    pub record: Record,
    /// Relevance in `0.0..=1.0`, comparable only within one result set.
    pub score: f32,
}

/// Stores and retrieves what the assistant should remember.
#[async_trait::async_trait]
pub trait Memory: Provider {
    /// Stores a record.
    ///
    /// # Errors
    ///
    /// Returns an error if the record is malformed for its scope or the
    /// backend rejects the write.
    async fn store(&self, record: Record) -> Result<()>;

    /// Retrieves the records best matching `query`, most relevant first.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable.
    async fn search(&self, query: Query) -> Result<Vec<Match>>;

    /// Deletes everything stored for a conversation.
    ///
    /// Called when a conversation ends and on explicit user request, so it
    /// must succeed even if nothing was stored.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable.
    async fn forget_conversation(&self, conversation: ConversationId) -> Result<()>;
}
