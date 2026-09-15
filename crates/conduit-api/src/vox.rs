//! Conduit-owned reverse proxy to a linked Vox peer.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue};
use axum::response::Response;
use conduit_provider::storage::{
    LinkedService, ProviderDefinitionVariant, ProviderSecret, SpeakerIdVariant,
};
use futures_util::TryStreamExt;

use crate::auth::ManagementCaller;
use crate::{ApiError, AppState};

/// `/vox/*` — Conduit-owned reverse proxy to the single linked Vox peer.
///
/// The browser stays on Conduit’s origin and never learns the peer’s API key.
pub async fn proxy(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, ApiError> {
    let peer = linked_peer(&state).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            ApiError::unavailable(format!("cannot build the Vox proxy client: {error}"))
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
    upstream = upstream.header(
        header::AUTHORIZATION,
        format!("Bearer {}", proxy_api_key(&state, &peer.provider_definition_id).await?),
    );
    upstream = upstream.body(reqwest::Body::wrap_stream(body.into_data_stream()));

    let upstream = upstream.send().await.map_err(|error| {
        ApiError::unavailable(format!(
            "could not reach linked Vox peer `{}`: {error}",
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
                response = response
                    .header(header::LOCATION, rewrite_location(&peer.peer_base_url, location)?);
            }
            continue;
        }
        response = response.header(name, value);
    }

    let body = Body::from_stream(
        upstream.bytes_stream().map_err(|error| std::io::Error::other(error.to_string())),
    );
    response.body(body).map_err(|error| {
        ApiError::unavailable(format!("could not build the Vox proxy response: {error}"))
    })
}

async fn linked_peer(state: &AppState) -> Result<LinkedService, ApiError> {
    let ids = state.linked_service_ids().await.map_err(store_failure)?;
    match ids.as_slice() {
        [] => Err(ApiError::not_found(
            "no Vox peer is linked; complete the link flow first".to_owned(),
        )),
        [peer_id] => state
            .linked_service(peer_id)
            .await
            .map_err(store_failure)?
            .ok_or_else(|| ApiError::not_found(format!("no vox link for peer `{peer_id}`"))),
        _ => Err(ApiError::conflict(
            "multiple Vox peers are linked; this branch proxies exactly one".to_owned(),
        )),
    }
}

async fn proxy_api_key(state: &AppState, provider_id: &str) -> Result<String, ApiError> {
    let definition =
        state.provider_definition(provider_id).await.map_err(store_failure)?.ok_or_else(
            || {
                ApiError::unprocessable(format!(
                    "linked Vox provider definition `{provider_id}` is missing"
                ))
            },
        )?;

    match definition.variant {
        ProviderDefinitionVariant::SpeakerId {
            variant:
                SpeakerIdVariant::Http { api_key: Some(ProviderSecret::Inline { value }), .. },
        } => Ok(value),
        _ => Err(ApiError::unprocessable(format!(
            "linked Vox provider definition `{provider_id}` does not hold an inline API key"
        ))),
    }
}

fn proxy_target_url(
    peer_base_url: &str,
    uri: &axum::http::Uri,
) -> Result<reqwest::Url, ApiError> {
    let mut base = reqwest::Url::parse(peer_base_url).map_err(|error| {
        ApiError::unprocessable(format!(
            "linked Vox base URL `{peer_base_url}` is invalid: {error}"
        ))
    })?;
    let original_path = uri.path();
    let forwarded_path = original_path
        .strip_prefix("/vox/")
        .or_else(|| (original_path == "/vox").then_some(""))
        .ok_or_else(|| {
            ApiError::not_found(format!("unsupported Vox route `{original_path}`"))
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

fn rewrite_location(peer_base_url: &str, location: &str) -> Result<HeaderValue, ApiError> {
    let base = reqwest::Url::parse(peer_base_url).map_err(|error| {
        ApiError::unprocessable(format!(
            "linked Vox base URL `{peer_base_url}` is invalid: {error}"
        ))
    })?;
    let rewritten = if location.starts_with('/') {
        format!("/vox{location}")
    } else if let Ok(resolved) = base.join(location) {
        if same_origin(&base, &resolved) {
            let mut rewritten = format!("/vox{}", strip_base_path(&base, resolved.path()));
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
        ApiError::unavailable(format!("linked Vox redirect could not be rewritten: {error}"))
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

fn store_failure(error: conduit_core::Error) -> ApiError {
    match error {
        conduit_core::Error::Config(detail) => ApiError::unprocessable(detail),
        other => ApiError::unavailable(other.to_string()),
    }
}
