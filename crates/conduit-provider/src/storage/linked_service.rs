//! Links to a Conduit satellite service.
//!
//! One row per linked peer. An operator establishes a link once and the peer
//! then reverse-proxies its UI through Conduit and may run service-specific
//! side channels (Vox roster sync, Memoria memory surface, etc.); nothing
//! outside this crate cares that the storage is dual-schema. The interesting
//! rule is that the sync token is stored as a SHA-256 hash so a leaked
//! `link.json` on a Conduit host cannot be replayed against the peer without
//! also compromising the DB.
//!
//! The wire vocabulary (`LinkedServiceKind`, `LinkedServicePanel`,
//! `Reachability`, `LinkStatus`) lives in the `conduit-link` crate; storage
//! only re-exports what its rows carry.

use chrono::{DateTime, Utc};
use conduit_core::Result;
pub use conduit_link::LinkedServicePanel;
use conduit_link::{LinkedServiceKind, Reachability};
use serde::{Deserialize, Serialize};

fn default_service_kind() -> LinkedServiceKind {
    LinkedServiceKind::Vox
}

fn default_reachability() -> Reachability {
    Reachability::Unknown
}

/// One linked Vox peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedService {
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
    /// Bearer Conduit presents when proxying to this peer.
    ///
    /// Spec 0005 §Reverse-proxy contract strips `authorization` before
    /// forwarding — an operator's Conduit token must not leak to a linked
    /// service — so a peer that runs its own auth (like Vox with
    /// `SPEAKER_ID_API_KEY` or the auto-generated `local_api_key`) has no
    /// bearer to check against. This row-scoped bearer fills that gap: the
    /// proxy substitutes it into the outgoing request so the peer sees a
    /// key it knows. Stored plain rather than hashed because the peer
    /// verifies it by string compare, so a hash on our end would leave the
    /// proxy with nothing to present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_auth_bearer: Option<String>,
    /// Outcome of the most recent `GET {peer_base_url}/link/health` probe.
    ///
    /// Written by the create handler and by the startup probe sweep. A
    /// failed probe does NOT remove the row — a peer that's temporarily
    /// down should still surface in the console as unreachable rather than
    /// vanish. Existing stored rows deserialize as `Unknown`.
    #[serde(default = "default_reachability")]
    pub reachability: Reachability,
    /// When the most recent probe fired, or `None` if never probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_probed_at: Option<DateTime<Utc>>,
}

/// Somewhere Vox links are kept.
///
/// Deliberately shaped like the other stores — list, get, put, remove, keyed
/// by peer id — so that a deployment's choice of backend stays one decision
/// rather than one per kind of thing stored.
#[async_trait::async_trait]
pub trait LinkedServiceStore: Send + Sync + 'static {
    /// Peer ids in stable order.
    async fn list(&self) -> Result<Vec<String>>;

    /// Fetches one link.
    async fn get(&self, peer_id: &str) -> Result<Option<LinkedService>>;

    /// Stores a link, returning whether it replaced one.
    async fn put(&self, peer_id: &str, link: LinkedService) -> Result<bool>;

    /// Removes a link, returning whether it existed.
    async fn remove(&self, peer_id: &str) -> Result<bool>;
}
