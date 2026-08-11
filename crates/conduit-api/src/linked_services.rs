//! Generic linked services that contribute operator panels.
//!
//! A linked service establishes trust once, tells Conduit what panel it wants
//! surfaced, and can then be rendered through a Conduit-owned proxy path.

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use conduit_link::{LinkedServiceKind, LinkedServicePanel};
use conduit_provider::storage::LinkedService;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::{ApiError, AppState};

/// Body of `POST /v1/linked-services` — the peer's request to establish a link.
#[derive(Debug, Deserialize)]
pub struct CreateLinkedServiceRequest {
    /// What kind of service is linking (used for typed fallback panels).
    pub service_kind: LinkedServiceKind,
    /// Human-readable label the operator gave the peer.
    pub peer_name: String,
    /// Stable id the peer chose for itself.
    pub peer_id: String,
    /// Base URL Conduit reaches the peer at (proxy target).
    pub peer_base_url: String,
    /// Panel manifest the peer wants Conduit to surface.
    pub panel: LinkedServicePanel,
}

/// Body of the 201 response from `POST /v1/linked-services`.
#[derive(Debug, Serialize)]
pub struct CreateLinkedServiceResponse {
    /// Opaque bearer the peer stores locally and presents on peer-revoke.
    pub sync_token: String,
}

/// Body of `POST /v1/linked-services/probe` — a manual-add manifest lookup.
#[derive(Debug, Deserialize)]
pub struct ProbeLinkedServiceRequest {
    /// Base URL of the service to probe. Conduit fetches `{url}/manifest`.
    pub url: String,
}

/// A service's self-description, served at `GET /manifest`.
///
/// Every satellite exposes this so the operator can add it by URL from Console
/// and Conduit can register/proxy it without service-specific glue.
#[derive(Debug, Serialize, Deserialize)]
pub struct LinkedServiceManifest {
    /// What sort of service this is.
    pub service_kind: LinkedServiceKind,
    /// Stable id the service chose for itself.
    pub peer_id: String,
    /// Human-readable label the service suggests.
    pub peer_name: String,
    /// Base URL the service is reachable at, from Conduit's network vantage.
    pub peer_base_url: String,
    /// Panel the service asks Conduit to surface.
    pub panel: LinkedServicePanel,
}

/// One linked service row as rendered by the management API.
#[derive(Debug, Serialize)]
pub struct LinkedServiceView {
    /// Which kind of service this row represents.
    pub service_kind: LinkedServiceKind,
    /// Stable peer id.
    pub peer_id: String,
    /// Operator-visible peer label.
    pub peer_name: String,
    /// Base URL Conduit reaches the peer at.
    pub peer_base_url: String,
    /// Resolved panel manifest (inline or typed-kind fallback).
    pub panel: LinkedServicePanel,
    /// Operator credential name that authorised the link.
    pub granted_by: String,
    /// When the link was established.
    pub granted_at: chrono::DateTime<Utc>,
    /// Last time the peer was seen using its sync token, if ever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<chrono::DateTime<Utc>>,
}

impl TryFrom<LinkedService> for LinkedServiceView {
    type Error = ApiError;

    fn try_from(link: LinkedService) -> Result<Self, Self::Error> {
        let panel = panel_for(&link).ok_or_else(|| {
            ApiError::unprocessable(format!(
                "linked service `{}` has no panel to render",
                link.peer_id
            ))
        })?;
        Ok(Self {
            service_kind: link.service_kind,
            peer_id: link.peer_id,
            peer_name: link.peer_name,
            peer_base_url: link.peer_base_url,
            panel,
            granted_by: link.granted_by,
            granted_at: link.granted_at,
            last_seen: link.last_seen,
        })
    }
}

/// `POST /v1/linked-services` — creates a link and returns the sync token.
pub async fn create(
    ManagementCaller(caller): ManagementCaller,
    State(state): State<AppState>,
    JsonBody(request): JsonBody<CreateLinkedServiceRequest>,
) -> Result<(StatusCode, Json<CreateLinkedServiceResponse>), ApiError> {
    let peer_id = normalise_peer_id(&request.peer_id)?;
    let peer_name = trimmed_field("peer_name", &request.peer_name)?;
    let peer_base_url = trimmed_field("peer_base_url", &request.peer_base_url)?;
    let panel = normalise_panel(&request.panel)?;

    if state.linked_service(&peer_id).await.map_err(store_failure)?.is_some() {
        return Err(ApiError::conflict(format!(
            "linked service `{peer_id}` already exists; unlink it first"
        )));
    }

    let sync_token = mint_sync_token();
    let link = LinkedService {
        service_kind: request.service_kind,
        peer_id: peer_id.clone(),
        peer_name: peer_name.to_owned(),
        peer_base_url: peer_base_url.to_owned(),
        sync_token_hash: hash_token(&sync_token),
        provider_definition_id: String::new(),
        panel: Some(panel),
        granted_by: caller.name.clone(),
        granted_at: Utc::now(),
        last_seen: None,
    };
    state.put_linked_service(link).await.map_err(store_failure)?;
    Ok((StatusCode::CREATED, Json(CreateLinkedServiceResponse { sync_token })))
}

/// `GET /v1/linked-services` — lists linked peers with their resolved panels.
pub async fn list(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<Json<Vec<LinkedServiceView>>, ApiError> {
    let mut views = Vec::new();
    for id in state.linked_service_ids().await.map_err(store_failure)? {
        if let Some(link) = state.linked_service(&id).await.map_err(store_failure)? {
            if panel_for(&link).is_some() {
                views.push(LinkedServiceView::try_from(link)?);
            }
        }
    }
    Ok(Json(views))
}

/// `POST /v1/linked-services/probe` — fetch a peer's manifest for the
/// operator to confirm before creating the link.
pub async fn probe(
    _caller: ManagementCaller,
    JsonBody(request): JsonBody<ProbeLinkedServiceRequest>,
) -> Result<Json<LinkedServiceManifest>, ApiError> {
    let base = trimmed_field("url", &request.url)?;
    let base_url = reqwest::Url::parse(base).map_err(|error| {
        ApiError::unprocessable(format!("url `{base}` is invalid: {error}"))
    })?;
    let manifest_url = base_url.join("manifest").map_err(|error| {
        ApiError::unprocessable(format!("cannot derive manifest URL from `{base}`: {error}"))
    })?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            ApiError::unavailable(format!("cannot build probe client: {error}"))
        })?;
    let response = client.get(manifest_url.clone()).send().await.map_err(|error| {
        ApiError::unavailable(format!("could not reach `{manifest_url}`: {error}"))
    })?;
    if !response.status().is_success() {
        return Err(ApiError::unavailable(format!(
            "manifest at `{manifest_url}` returned {}",
            response.status()
        )));
    }
    let manifest: LinkedServiceManifest = response.json().await.map_err(|error| {
        ApiError::unprocessable(format!(
            "manifest at `{manifest_url}` did not parse: {error}"
        ))
    })?;
    Ok(Json(manifest))
}

/// `DELETE /v1/linked-services/{peer_id}` — operator-authenticated unlink.
pub async fn delete(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let peer_id = normalise_peer_id(&peer_id)?;
    if state.remove_linked_service(&peer_id).await.map_err(store_failure)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no linked service for peer `{peer_id}`")))
    }
}

/// `DELETE /v1/linked-services/{peer_id}` with bearer — peer-initiated revoke.
pub async fn revoke(
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let peer_id = normalise_peer_id(&peer_id)?;
    let Some(link) = state.linked_service(&peer_id).await.map_err(store_failure)? else {
        return Err(ApiError::not_found(format!("no linked service for peer `{peer_id}`")));
    };
    let token = bearer(&headers).ok_or_else(ApiError::unauthorized)?;
    if hash_token(token) != link.sync_token_hash {
        return Err(ApiError::unauthorized());
    }
    state.remove_linked_service(&peer_id).await.map_err(store_failure)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `ANY /linked-services/{peer_id}/{*rest}` — reverse proxy to the peer.
pub async fn proxy(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, ApiError> {
    let peer = linked_peer(&state, request.uri().path()).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            ApiError::unavailable(format!(
                "cannot build the linked-service proxy client: {error}"
            ))
        })?;

    let (parts, body) = request.into_parts();
    let target = proxy_target_url(&peer.peer_base_url, &parts.uri)?;
    let mut upstream = client.request(parts.method, target);
    for (name, value) in &parts.headers {
        if name == header::AUTHORIZATION
            || name == header::HOST
            || name == header::CONTENT_LENGTH
            || name == header::CONNECTION
        {
            continue;
        }
        upstream = upstream.header(name, value);
    }
    upstream = upstream.body(reqwest::Body::wrap_stream(body.into_data_stream()));

    let upstream = upstream.send().await.map_err(|error| {
        ApiError::unavailable(format!(
            "could not reach linked service `{}`: {error}",
            peer.peer_id
        ))
    })?;

    let status = upstream.status();
    let mut response = Response::builder().status(status);
    for (name, value) in upstream.headers() {
        if name == header::CONTENT_LENGTH || name == header::CONNECTION {
            continue;
        }
        if name == header::LOCATION {
            if let Ok(location) = value.to_str() {
                response = response.header(
                    header::LOCATION,
                    rewrite_location(&peer.peer_id, &peer.peer_base_url, location)?,
                );
            }
            continue;
        }
        response = response.header(name, value);
    }

    let body = Body::from_stream(
        upstream.bytes_stream().map_err(|error| std::io::Error::other(error.to_string())),
    );
    response.body(body).map_err(|error| {
        ApiError::unavailable(format!(
            "could not build the linked-service proxy response: {error}"
        ))
    })
}

async fn linked_peer(state: &AppState, path: &str) -> Result<LinkedService, ApiError> {
    let peer_id = path
        .strip_prefix("/linked-services/")
        .and_then(|rest| rest.split('/').next())
        .ok_or_else(|| {
            ApiError::not_found(format!("unsupported linked-service route `{path}`"))
        })?;
    let peer_id = normalise_peer_id(peer_id)?;
    state
        .linked_service(&peer_id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no linked service for peer `{peer_id}`")))
}

fn proxy_target_url(
    peer_base_url: &str,
    uri: &axum::http::Uri,
) -> Result<reqwest::Url, ApiError> {
    let mut base = reqwest::Url::parse(peer_base_url).map_err(|error| {
        ApiError::unprocessable(format!(
            "linked service base URL `{peer_base_url}` is invalid: {error}"
        ))
    })?;
    let original_path = uri.path();
    let forwarded_path = original_path
        .strip_prefix("/linked-services/")
        .and_then(|rest| {
            let mut segments = rest.splitn(2, '/');
            let _peer_id = segments.next()?;
            Some(segments.next().unwrap_or(""))
        })
        .ok_or_else(|| {
            ApiError::not_found(format!("unsupported linked-service route `{original_path}`"))
        })?;
    let base_path = base.path().trim_end_matches('/');
    let joined_path = if forwarded_path.is_empty() {
        if base_path.is_empty() {
            "/".to_owned()
        } else {
            base_path.to_owned()
        }
    } else if base_path.is_empty() {
        format!("/{forwarded_path}")
    } else {
        format!("{base_path}/{forwarded_path}")
    };
    base.set_path(&joined_path);
    base.set_query(uri.query());
    Ok(base)
}

fn rewrite_location(
    peer_id: &str,
    peer_base_url: &str,
    location: &str,
) -> Result<HeaderValue, ApiError> {
    let base = reqwest::Url::parse(peer_base_url).map_err(|error| {
        ApiError::unprocessable(format!(
            "linked service base URL `{peer_base_url}` is invalid: {error}"
        ))
    })?;
    let rewritten = if location.starts_with('/') {
        format!("/linked-services/{peer_id}{location}")
    } else if let Ok(resolved) = base.join(location) {
        if same_origin(&base, &resolved) {
            let mut rewritten = format!(
                "/linked-services/{peer_id}{}",
                strip_base_path(&base, resolved.path())
            );
            if let Some(query) = resolved.query() {
                rewritten.push('?');
                rewritten.push_str(query);
            }
            rewritten
        } else {
            location.to_owned()
        }
    } else {
        location.to_owned()
    };
    HeaderValue::from_str(&rewritten).map_err(|error| {
        ApiError::unavailable(format!(
            "linked-service redirect could not be rewritten: {error}"
        ))
    })
}

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

fn strip_base_path(base: &reqwest::Url, path: &str) -> String {
    let prefix = base.path().trim_end_matches('/');
    if prefix.is_empty() {
        path.to_owned()
    } else {
        let stripped = path.strip_prefix(prefix).unwrap_or(path);
        if stripped.is_empty() {
            "/".to_owned()
        } else if stripped.starts_with('/') {
            stripped.to_owned()
        } else {
            format!("/{stripped}")
        }
    }
}

fn normalise_peer_id(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::unprocessable("peer_id cannot be empty"));
    }
    let normalised = trimmed.to_ascii_lowercase();
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

fn normalise_panel(panel: &LinkedServicePanel) -> Result<LinkedServicePanel, ApiError> {
    let id = trimmed_field("panel.id", &panel.id)?.to_ascii_lowercase();
    let label = trimmed_field("panel.label", &panel.label)?.to_owned();
    let icon = trimmed_field("panel.icon", &panel.icon)?.to_ascii_lowercase();
    let path = trimmed_field("panel.path", &panel.path)?;
    if !path.starts_with('/') {
        return Err(ApiError::unprocessable("panel.path must start with `/`"));
    }
    Ok(LinkedServicePanel { id, label, icon, path: path.to_owned() })
}

fn panel_for(link: &LinkedService) -> Option<LinkedServicePanel> {
    link.panel.clone().or_else(|| match link.service_kind {
        // Backward compatibility for Vox links stored before panel manifests
        // existed: they were real linked peers and should still surface a tab.
        LinkedServiceKind::Vox => Some(LinkedServicePanel {
            id: "vox".to_owned(),
            label: "Vox".to_owned(),
            icon: "users".to_owned(),
            path: "/ui/".to_owned(),
        }),
        LinkedServiceKind::Memoria => Some(LinkedServicePanel {
            id: "memoria".to_owned(),
            label: "Memoria".to_owned(),
            icon: "brain".to_owned(),
            path: "/ui/".to_owned(),
        }),
        LinkedServiceKind::Instrumenta => Some(LinkedServicePanel {
            id: "instrumenta".to_owned(),
            label: "Instrumenta".to_owned(),
            icon: "code".to_owned(),
            path: "/ui/".to_owned(),
        }),
        LinkedServiceKind::Excita => Some(LinkedServicePanel {
            id: "excita".to_owned(),
            label: "Excita".to_owned(),
            icon: "radio".to_owned(),
            path: "/ui/".to_owned(),
        }),
        LinkedServiceKind::Dicta => Some(LinkedServicePanel {
            id: "dicta".to_owned(),
            label: "Dicta".to_owned(),
            icon: "wand".to_owned(),
            path: "/ui/".to_owned(),
        }),
        LinkedServiceKind::Forma => Some(LinkedServicePanel {
            id: "forma".to_owned(),
            label: "Forma".to_owned(),
            icon: "shapes".to_owned(),
            path: "/ui/".to_owned(),
        }),
        LinkedServiceKind::Generic => None,
    })
}

fn mint_sync_token() -> String {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Hex-encoded SHA-256 of a sync token, used for storage and bearer compare.
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

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))
}
