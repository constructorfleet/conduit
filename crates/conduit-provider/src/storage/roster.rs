//! The people a deployment has enrolled.
//!
//! An identification service stores voice prints under a label and answers
//! with that label. Conduit owns the label — a [`SpeakerId`] — precisely so
//! that the service never holds a person's name, and so a deployment can
//! change embedding models without every enrolled voice becoming a stranger.
//!
//! The consequence is that *somebody* has to remember which id is whose, and
//! that is what this store is. It is the only place a name is written down,
//! and it is what an operator screen lists.

use chrono::{DateTime, Utc};
use conduit_core::id::SpeakerId;
use conduit_core::Result;
use serde::{Deserialize, Serialize};

/// One enrolled — or merely named — person.
///
/// A roster entry is created before any audio is: an operator names somebody,
/// and then enrolls their voice, possibly across several sittings. So an entry
/// with `samples` of zero is normal and means "named, not yet heard".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolledSpeaker {
    /// The identifier Conduit owns and the identification service stores as an
    /// opaque label.
    pub id: SpeakerId,
    /// What a person calls them.
    ///
    /// Free text rather than a validated name: this is never a storage key —
    /// the id is — so it may hold whatever an operator would actually say.
    pub name: String,
    /// How many utterances have been accepted for this voice.
    ///
    /// Counted here rather than asked of the service, because the services
    /// Conduit speaks to do not all report it, and an operator deciding
    /// whether to record another sample needs the number either way.
    #[serde(default)]
    pub samples: u32,
    /// The provider definition the voice prints were enrolled against.
    ///
    /// Recorded because a voice print does not travel: a deployment with two
    /// identification services has enrolled this person on one of them, and an
    /// entry that did not say which would look enrolled everywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// When the entry was created.
    pub created_at: DateTime<Utc>,
    /// When a sample was last accepted, or `None` if none ever was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled_at: Option<DateTime<Utc>>,
}

impl EnrolledSpeaker {
    /// A named person nobody has recorded yet.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            id: SpeakerId::new(),
            name: name.into(),
            samples: 0,
            provider: None,
            created_at: Utc::now(),
            enrolled_at: None,
        }
    }

    /// Whether any sample has been accepted for this voice.
    ///
    /// A named entry with no samples identifies nobody: the service has never
    /// heard them, so their name would never come back from a turn.
    #[must_use]
    pub const fn is_enrolled(&self) -> bool {
        self.samples > 0
    }
}

/// Somewhere the roster is kept.
///
/// Deliberately shaped like the other stores — list, get, put, remove, keyed
/// by a string — so that a deployment's choice of backend stays one decision
/// rather than one per kind of thing stored.
#[async_trait::async_trait]
pub trait SpeakerRosterStore: Send + Sync + 'static {
    /// Speaker ids in stable order.
    async fn list(&self) -> Result<Vec<String>>;

    /// Fetches one roster entry.
    async fn get(&self, id: &str) -> Result<Option<EnrolledSpeaker>>;

    /// Stores a roster entry, returning whether it replaced one.
    async fn put(&self, id: &str, speaker: EnrolledSpeaker) -> Result<bool>;

    /// Removes a roster entry, returning whether it existed.
    async fn remove(&self, id: &str) -> Result<bool>;
}
