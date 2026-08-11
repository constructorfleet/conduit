//! Wire types for the Conduit bidirectional link protocol (spec 0005).
//!
//! This crate is the shared vocabulary — no client, no handlers, no HTTP.
//! Handlers live in `conduit-api::linked_services`; storage rows live in
//! `conduit-provider::storage`; both import the types below rather than
//! defining their own.

use serde::{Deserialize, Serialize};

/// What sort of linked service this row represents.
///
/// Every typed variant earns a stable fallback panel (see spec 0005 §Panel
/// manifest); `Generic` is the escape hatch for anything without a typed
/// kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedServiceKind {
    /// Vox speaker identification and enrollment.
    Vox,
    /// Memoria memory / MCP surface.
    Memoria,
    /// Instrumenta tool / integration surface.
    Instrumenta,
    /// Excita wakeword and management.
    Excita,
    /// Dicta utterance transform surface.
    Dicta,
    /// Forma rule engine, once it runs as a standalone linked service.
    Forma,
    /// Any other linked service without a typed fallback panel.
    Generic,
}

impl LinkedServiceKind {
    /// Snake-case wire name matching the serde encoding.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vox => "vox",
            Self::Memoria => "memoria",
            Self::Instrumenta => "instrumenta",
            Self::Excita => "excita",
            Self::Dicta => "dicta",
            Self::Forma => "forma",
            Self::Generic => "generic",
        }
    }
}

/// An operator panel a linked service wants Conduit to surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedServicePanel {
    /// Stable identifier for the panel within this service.
    pub id: String,
    /// Operator-visible tab label.
    pub label: String,
    /// Icon name the console should render for this panel.
    pub icon: String,
    /// Upstream path, rooted at the peer base URL.
    pub path: String,
}

/// Whether Conduit can reach the peer right now (spec 0005 §Reachability).
///
/// This is the base-protocol health signal only; side-channel failures MUST
/// NOT flip this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    /// Not yet probed.
    Unknown,
    /// Last probe succeeded.
    Ok,
    /// Last probe failed (timeout, non-2xx, connection error).
    Unreachable,
}

/// Snapshot of a link's operator-visible state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkStatus {
    /// Peer id this status describes.
    pub peer_id: String,
    /// Human-readable label for the peer.
    pub peer_name: String,
    /// Whether Conduit can currently reach the peer.
    pub reachability: Reachability,
    /// ISO-8601 timestamp of the last successful contact, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn service_kind_serialises_snake_case() {
        assert_eq!(serde_json::to_value(LinkedServiceKind::Dicta).unwrap(), json!("dicta"),);
        assert_eq!(serde_json::to_value(LinkedServiceKind::Forma).unwrap(), json!("forma"),);
    }

    #[test]
    fn service_kind_as_str_matches_serde() {
        for kind in [
            LinkedServiceKind::Vox,
            LinkedServiceKind::Memoria,
            LinkedServiceKind::Instrumenta,
            LinkedServiceKind::Excita,
            LinkedServiceKind::Dicta,
            LinkedServiceKind::Forma,
            LinkedServiceKind::Generic,
        ] {
            let via_serde = serde_json::to_value(kind).unwrap();
            assert_eq!(via_serde, json!(kind.as_str()));
        }
    }

    #[test]
    fn panel_round_trips() {
        let panel = LinkedServicePanel {
            id: "vox".into(),
            label: "Voices".into(),
            icon: "mic".into(),
            path: "/ui/".into(),
        };
        let value = serde_json::to_value(&panel).unwrap();
        assert_eq!(value, json!({"id":"vox","label":"Voices","icon":"mic","path":"/ui/"}),);
        let back: LinkedServicePanel = serde_json::from_value(value).unwrap();
        assert_eq!(back, panel);
    }

    #[test]
    fn reachability_serialises_snake_case() {
        assert_eq!(serde_json::to_value(Reachability::Ok).unwrap(), json!("ok"),);
        assert_eq!(
            serde_json::to_value(Reachability::Unreachable).unwrap(),
            json!("unreachable"),
        );
        assert_eq!(serde_json::to_value(Reachability::Unknown).unwrap(), json!("unknown"),);
    }
}
