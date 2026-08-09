//! Links to Conduit Vox peers.
//!
//! An operator establishes a link once — `POST /v1/vox/links` — and Conduit
//! answers with a scoped sync token the Vox peer keeps and an
//! `http_speaker_id` provider definition it auto-provisioned to point at that
//! peer. The link row survives restarts; the raw sync token does not, so a
//! read of the row returns its hash rather than the token itself.
//!
//! The reverse proxy that consumes `peer_base_url` and the auth extractor that
//! recognises sync tokens both land in later commits — no caller uses one yet.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, ProviderSecret, SpeakerEngine,
    SpeakerIdVariant, VoxLink,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::{ApiError, AppState};

/// What an operator sends to establish a link.
#[derive(Debug, Deserialize)]
pub struct CreateVoxLinkRequest {
    /// Human-readable label an operator gave the peer.
    pub peer_name: String,
    /// Stable identifier the Vox peer chose for itself.
    pub peer_id: String,
    /// Base URL Conduit reaches the peer at.
    pub vox_base_url: String,
    /// API key the peer accepts on its own routes.
    pub vox_api_key: String,
}

/// What a successful link returns.
///
/// The raw sync token is shown exactly once — the row only carries its hash.
#[derive(Debug, Serialize)]
pub struct CreateVoxLinkResponse {
    /// Raw sync token. Given to the caller once; store it on the Vox side.
    pub sync_token: String,
    /// Provider definition id Conduit auto-provisioned for this peer.
    pub provider_definition_id: String,
}

/// A link as rendered through the management API. Never carries a raw token.
#[derive(Debug, Serialize)]
pub struct VoxLinkView {
    /// Stable identifier the Vox peer chose for itself.
    pub peer_id: String,
    /// Human-readable label an operator gave the peer.
    pub peer_name: String,
    /// Base URL Conduit reaches the peer at.
    pub peer_base_url: String,
    /// Provider definition id Conduit auto-provisioned for this peer.
    pub provider_definition_id: String,
    /// Name of the operator credential that authorised the link.
    pub granted_by: String,
    /// When the link was established.
    pub granted_at: chrono::DateTime<Utc>,
    /// Last time the peer was seen using its sync token, or `null` if never.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<chrono::DateTime<Utc>>,
}

impl From<VoxLink> for VoxLinkView {
    fn from(link: VoxLink) -> Self {
        Self {
            peer_id: link.peer_id,
            peer_name: link.peer_name,
            peer_base_url: link.peer_base_url,
            provider_definition_id: link.provider_definition_id,
            granted_by: link.granted_by,
            granted_at: link.granted_at,
            last_seen: link.last_seen,
        }
    }
}

/// `POST /v1/vox/links` — link a Conduit Vox peer.
///
/// Mints a sync token, stores the link row with only the token's hash, and
/// auto-provisions the `http_speaker_id` provider definition that pipelines
/// will name to reach this peer.
pub async fn create(
    ManagementCaller(caller): ManagementCaller,
    State(state): State<AppState>,
    JsonBody(request): JsonBody<CreateVoxLinkRequest>,
) -> Result<(StatusCode, Json<CreateVoxLinkResponse>), ApiError> {
    let peer_id = normalise_peer_id(&request.peer_id)?;
    let peer_name = trimmed_field("peer_name", &request.peer_name)?;
    let vox_base_url = trimmed_field("vox_base_url", &request.vox_base_url)?;
    let vox_api_key = trimmed_field("vox_api_key", &request.vox_api_key)?;

    let provider_definition_id = provider_id_for(&peer_id);
    // Refuses replacing an existing definition an operator wrote by hand:
    // silently overwriting a hand-authored provider is worse than refusing
    // the link and telling the operator what is in the way.
    if state
        .provider_definition(&provider_definition_id)
        .await
        .map_err(store_failure)?
        .is_some()
    {
        return Err(ApiError::conflict(format!(
            "provider definition `{provider_definition_id}` already exists; \
             remove or rename it before linking this Vox peer"
        )));
    }

    let sync_token = mint_sync_token();
    let sync_token_hash = hash_token(&sync_token);

    let link = VoxLink {
        peer_id: peer_id.clone(),
        peer_name: peer_name.to_owned(),
        peer_base_url: vox_base_url.to_owned(),
        sync_token_hash,
        provider_definition_id: provider_definition_id.clone(),
        granted_by: caller.name.clone(),
        granted_at: Utc::now(),
        last_seen: None,
    };

    let definition = ProviderDefinition {
        id: provider_definition_id.clone(),
        label: format!("Conduit Vox — {peer_name}"),
        variant: ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::Http {
                base_url: vox_base_url.to_owned(),
                api_key: Some(ProviderSecret::Inline { value: vox_api_key.to_owned() }),
                // A default until an operator sets the engine on the peer
                // through the Providers screen: Vox reports what it is
                // actually running through its own /health, and a definition
                // that guessed wrong here identifies the same voice under a
                // different label anyway.
                engine: SpeakerEngine::SpeechBrain,
                threshold_percent: conduit_provider::storage::DEFAULT_THRESHOLD_PERCENT,
            },
        },
        settings: serde_json::Map::new(),
    };

    // Order matters: the definition is written first so a failure mid-link
    // leaves an inert provider rather than a link row pointing at a
    // definition that does not exist.
    state
        .put_provider_definition(&provider_definition_id, definition)
        .await
        .map_err(store_failure)?;
    state.put_vox_link(link).await.map_err(|error| {
        // Best-effort rollback: if the link row cannot be written, undo the
        // provider write so a retry does not trip the "already exists" refusal.
        let state = state.clone();
        let provider_id = provider_definition_id.clone();
        tokio::spawn(async move {
            if let Err(cleanup) = state.remove_provider_definition(&provider_id).await {
                tracing::warn!(
                    provider = %provider_id,
                    %cleanup,
                    "failed to roll back auto-provisioned provider after link write failure"
                );
            }
        });
        store_failure(error)
    })?;

    tracing::info!(
        peer = %peer_id,
        peer_name = %peer_name,
        provider = %provider_definition_id,
        granted_by = %caller.name,
        "linked a Conduit Vox peer"
    );

    Ok((
        StatusCode::CREATED,
        Json(CreateVoxLinkResponse { sync_token, provider_definition_id }),
    ))
}

/// `GET /v1/vox/links` — list linked peers, redacted.
pub async fn list(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<Json<Vec<VoxLinkView>>, ApiError> {
    let mut views = Vec::new();
    for id in state.vox_link_ids().await.map_err(store_failure)? {
        if let Some(link) = state.vox_link(&id).await.map_err(store_failure)? {
            views.push(link.into());
        }
    }
    Ok(Json(views))
}

/// `DELETE /v1/vox/links/{peer_id}` — revoke a sync token.
///
/// Removes the link row so the sync token stops working. Deliberately leaves
/// the auto-provisioned provider definition in place: an operator may still
/// want to talk to the same Vox with a different link, and a delete that
/// tore out a working provider would break every pipeline that named it.
pub async fn delete(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let peer_id = normalise_peer_id(&peer_id)?;
    if state.remove_vox_link(&peer_id).await.map_err(store_failure)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no vox link for peer `{peer_id}`")))
    }
}

/// The provider definition id auto-provisioned for a peer.
fn provider_id_for(peer_id: &str) -> String {
    format!("vox-{peer_id}")
}

/// Sanity-checks a peer id, folding it to lowercase and refusing anything the
/// storage layer would refuse. Kept here rather than at the store so the
/// error is about the request field, not about a random name check.
fn normalise_peer_id(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::unprocessable("peer_id cannot be empty"));
    }
    let normalised = trimmed.to_ascii_lowercase();
    // Same rules as the storage layer: alphanumerics, dash, underscore. Named
    // fields say what the caller did wrong rather than pointing at an inner
    // storage error.
    if normalised.len() > 128
        || !normalised.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-' | '_'))
    {
        return Err(ApiError::unprocessable(
            "peer_id must be 1-128 characters of lowercase letters, digits, `-`, or `_`",
        ));
    }
    Ok(normalised)
}

fn trimmed_field<'a>(name: &str, value: &'a str) -> Result<&'a str, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::unprocessable(format!("{name} cannot be empty")));
    }
    Ok(trimmed)
}

/// Two UUIDs joined and base64-URL-encoded: 256 bits of entropy in a form the
/// Vox operator can paste as a bearer token.
fn mint_sync_token() -> String {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Hex-encoded SHA-256 of `token`.
///
/// Public so a later commit's auth extractor can hash a presented token and
/// look it up in the store.
#[must_use]
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn store_failure(error: conduit_core::Error) -> ApiError {
    match error {
        conduit_core::Error::Config(detail) => ApiError::unprocessable(detail),
        other => ApiError::unavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_deterministic_and_hex() {
        let hash = hash_token("hello");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash, hash_token("hello"));
        assert_ne!(hash, hash_token("HELLO"));
    }

    #[test]
    fn minted_tokens_are_distinct_and_long_enough() {
        let a = mint_sync_token();
        let b = mint_sync_token();
        assert_ne!(a, b, "two mints must not collide");
        assert!(a.len() >= 40, "at least 32 bytes of entropy in the token");
    }

    #[test]
    fn peer_id_normalisation_lowercases_and_trims() {
        assert_eq!(normalise_peer_id("  ABC-01  ").unwrap(), "abc-01");
    }

    #[test]
    fn peer_id_refuses_path_traversal_and_odd_characters() {
        for id in ["", "  ", "../etc", "abc/def", "abc def", "a\0b"] {
            assert!(normalise_peer_id(id).is_err(), "{id} should be rejected");
        }
    }
}
