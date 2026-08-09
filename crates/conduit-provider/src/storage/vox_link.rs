//! Links to a Conduit Vox instance.
//!
//! One row per linked Vox peer. An operator establishes a link once and Vox
//! then syncs the roster and reverse-proxies its UI through Conduit; nothing
//! outside this crate cares that the storage is dual-schema. The interesting
//! rule is that the sync token is stored as a SHA-256 hash so a leaked
//! `link.json` on a Conduit host cannot be replayed against `/v1/speakers`
//! without also compromising the DB.

use chrono::{DateTime, Utc};
use conduit_core::Result;
use serde::{Deserialize, Serialize};

/// What sort of linked service this row represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedServiceKind {
    Vox,
    Memoria,
    Instrumenta,
    Excita,
    Generic,
}

impl LinkedServiceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vox => "vox",
            Self::Memoria => "memoria",
            Self::Instrumenta => "instrumenta",
            Self::Excita => "excita",
            Self::Generic => "generic",
        }
    }
}

fn default_service_kind() -> LinkedServiceKind {
    LinkedServiceKind::Vox
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

/// One linked Vox peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoxLink {
    /// What kind of service this row represents.
    ///
    /// Defaults to `vox` so existing stored rows continue to deserialize as the
    /// only kind Conduit knew about when they were written.
    #[serde(default = "default_service_kind")]
    pub service_kind: LinkedServiceKind,
    /// Stable identifier the Vox peer chose for itself.
    ///
    /// Used as the row key so a peer that reboots and re-links replaces its
    /// own row rather than accumulating stale ones.
    pub peer_id: String,
    /// Human-readable label an operator gave the peer.
    pub peer_name: String,
    /// Base URL Conduit reaches the peer at (`/vox/*` reverse proxy target).
    pub peer_base_url: String,
    /// Hex-encoded SHA-256 of the minted sync token. The raw token is
    /// returned to the caller of `POST /v1/vox/links` and never stored.
    pub sync_token_hash: String,
    /// Provider definition id auto-provisioned for this peer.
    ///
    /// Recorded so an operator screen can point at the auto-provisioned
    /// `http_speaker_id` definition, and so a revoke can name it in warnings.
    pub provider_definition_id: String,
    /// Panel the service asked Conduit to surface, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<LinkedServicePanel>,
    /// Name of the operator credential that authorised the link, for logs.
    pub granted_by: String,
    /// When the link was established.
    pub granted_at: DateTime<Utc>,
    /// Last time the peer was seen using its sync token, or `None` if never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<DateTime<Utc>>,
}

/// Somewhere Vox links are kept.
///
/// Deliberately shaped like the other stores — list, get, put, remove, keyed
/// by peer id — so that a deployment's choice of backend stays one decision
/// rather than one per kind of thing stored.
#[async_trait::async_trait]
pub trait VoxLinkStore: Send + Sync + 'static {
    /// Peer ids in stable order.
    async fn list(&self) -> Result<Vec<String>>;

    /// Fetches one link.
    async fn get(&self, peer_id: &str) -> Result<Option<VoxLink>>;

    /// Stores a link, returning whether it replaced one.
    async fn put(&self, peer_id: &str, link: VoxLink) -> Result<bool>;

    /// Removes a link, returning whether it existed.
    async fn remove(&self, peer_id: &str) -> Result<bool>;
}
