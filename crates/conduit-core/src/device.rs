//! Device-facing conversation protocol.
//!
//! A Conduit device speaks a deliberately small WebSocket protocol: binary
//! frames carry audio in both directions, and text frames carry these JSON
//! control messages.

use serde::{Deserialize, Serialize};

use crate::id::ConversationId;

/// What a client can say that is not audio.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// The utterance is over; answer it.
    End,
}

/// What the server says that is not audio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notice {
    /// Sent before any audio, so the client can follow its own events.
    Started {
        /// The conversation this turn is filed under.
        conversation: ConversationId,
    },
    /// The turn finished normally.
    Done,
    /// The turn failed. The detail is also on the event stream.
    Failed {
        /// What went wrong.
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_command_matches_the_wire_protocol() {
        let json = serde_json::to_string(&Command::End).expect("serializes");
        assert_eq!(json, r#"{"type":"end"}"#);
        assert_eq!(serde_json::from_str::<Command>(&json).expect("parses"), Command::End);
    }

    #[test]
    fn notices_match_the_wire_protocol() {
        let done = serde_json::to_value(Notice::Done).expect("serializes");
        assert_eq!(done["type"], "done");

        let failed = serde_json::to_value(Notice::Failed { error: "offline".into() })
            .expect("serializes");
        assert_eq!(failed["type"], "failed");
        assert_eq!(failed["error"], "offline");
    }
}
