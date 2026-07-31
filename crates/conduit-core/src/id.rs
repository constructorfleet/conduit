//! Strongly-typed identifiers.
//!
//! Every identifier is a distinct type so that a [`DeviceId`] can never be
//! passed where a [`ConversationId`] is expected. All of them wrap a UUID v4
//! and serialize transparently as a string.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a new random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing UUID.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Returns the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

define_id!(
    /// Identifies a single emitted event.
    EventId
);
define_id!(
    /// Identifies a multi-turn conversation.
    ConversationId
);
define_id!(
    /// Identifies one request/response turn within a conversation.
    TurnId
);
define_id!(
    /// Identifies a physical or virtual audio endpoint.
    DeviceId
);
define_id!(
    /// Identifies an enrolled speaker voice print.
    SpeakerId
);
define_id!(
    /// Correlates every event belonging to one trip through the pipeline.
    TraceId
);

/// Identifies a single tool invocation.
///
/// Unlike every other identifier here this wraps an opaque string rather than
/// a UUID, because the language model issues it — `call_abc123` — and expects
/// it echoed back verbatim on the matching result. A generated id could not be
/// translated back, so the vendor's own is what travels through the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Wraps an identifier issued by a provider.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Generates an identifier, for providers that issue none of their own.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// The identifier as the provider wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<String> for ToolCallId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for ToolCallId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_ids_keep_the_providers_own_string() {
        let id = ToolCallId::new("call_abc123");
        assert_eq!(id.as_str(), "call_abc123");
        // It must survive a round trip unchanged, or the result cannot be
        // matched to the call that produced it.
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, "\"call_abc123\"");
        assert_eq!(serde_json::from_str::<ToolCallId>(&json).expect("deserialize"), id);
    }

    #[test]
    fn generated_tool_call_ids_are_unique() {
        assert_ne!(ToolCallId::generate(), ToolCallId::generate());
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(ConversationId::new(), ConversationId::new());
    }

    #[test]
    fn ids_round_trip_as_bare_strings() {
        let id = DeviceId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<DeviceId>(&json).expect("deserialize"), id);
    }
}
