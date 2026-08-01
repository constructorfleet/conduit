//! Provider definition endpoints.

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use conduit_provider::storage::{
    ProviderCapability, ProviderDefinition, ProviderDefinitionVariant,
};
use conduit_provider::Health;
use serde::Serialize;

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::pipelines::{component_catalog, PipelineComponentCatalog};
use crate::status::{ProviderKind, ProviderStatus, ProviderStatusState};
use crate::{ApiError, AppState};

/// A provider definition as rendered through the management API.
#[derive(Debug, Serialize)]
pub struct ProviderDefinitionView {
    /// Stable provider id referenced by pipeline graph nodes.
    pub id: String,
    /// Human-readable label for operator screens.
    pub label: String,
    /// Runtime capability supplied by this definition.
    pub kind: ProviderCapability,
    /// Typed provider-specific settings, with inline secrets redacted.
    pub variant: conduit_provider::storage::ProviderDefinitionVariant,
}

impl From<ProviderDefinition> for ProviderDefinitionView {
    fn from(definition: ProviderDefinition) -> Self {
        let definition = definition.redacted();
        Self {
            id: definition.id,
            label: definition.label,
            kind: definition.variant.capability(),
            variant: definition.variant,
        }
    }
}

/// `GET /v1/catalog/providers` — provider component catalog.
pub async fn catalog(_caller: ManagementCaller) -> Json<PipelineComponentCatalog> {
    Json(PipelineComponentCatalog { components: component_catalog() })
}

/// `GET /v1/providers` — ids of every provider definition.
pub async fn list(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, ApiError> {
    state.provider_definition_ids().await.map(Json).map_err(store_failure)
}

/// `GET /v1/providers/{id}` — one provider definition with redacted secrets.
pub async fn get(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderDefinitionView>, ApiError> {
    let definition = state
        .provider_definition(&id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no provider definition `{id}`")))?;
    Ok(Json(definition.into()))
}

/// `PUT /v1/providers/{id}` — creates or replaces one provider definition.
pub async fn put(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(definition): JsonBody<ProviderDefinition>,
) -> Result<(StatusCode, Json<ProviderDefinitionView>), ApiError> {
    if definition.id != id {
        return Err(ApiError::unprocessable(format!(
            "provider definition id `{}` does not match route id `{id}`",
            definition.id
        )));
    }
    let existing = state.provider_definition(&id).await.map_err(store_failure)?;
    let definition = definition.with_secret_updates_from(existing.as_ref());
    validate_provider_definition(&definition)?;
    let replaced =
        state.put_provider_definition(&id, definition.clone()).await.map_err(store_failure)?;
    let status = if replaced { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(definition.into())))
}

/// `DELETE /v1/providers/{id}` — removes one provider definition when unused.
pub async fn delete(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let affected = affected_pipelines(&state, &id).await?;
    if !affected.is_empty() {
        return Ok((
            StatusCode::CONFLICT,
            Json(DeleteConflict {
                error: "conflict",
                detail: "provider definition is still referenced by pipelines",
                affected_pipelines: affected,
            }),
        )
            .into_response());
    }

    if state.remove_provider_definition(&id).await.map_err(store_failure)? {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(ApiError::not_found(format!("no provider definition `{id}`")))
    }
}

/// `POST /v1/providers/{id}/test` — active reachability check for one provider.
pub async fn test(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderStatus>, ApiError> {
    let definition = state
        .provider_definition(&id)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no provider definition `{id}`")))?;
    let kind = provider_kind(definition.capability());
    let affected_pipelines = affected_pipelines(&state, &id).await?;
    let Some(providers) = state.providers() else {
        return Ok(Json(unregistered_status(kind, id, affected_pipelines)));
    };

    let health = match definition.capability() {
        ProviderCapability::Stt => match providers.stt().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::Llm => match providers.llm().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::Tts => match providers.tts().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
        ProviderCapability::Tool => match providers.tools().get(&id) {
            Some(provider) => Some(provider.health().await),
            None => None,
        },
    };

    let Some(health) = health else {
        return Ok(Json(unregistered_status(kind, id, affected_pipelines)));
    };

    state.record_provider_reachability(&id, health.clone());
    Ok(Json(status_from_health(kind, id, health, affected_pipelines)))
}

#[derive(Serialize)]
struct DeleteConflict {
    error: &'static str,
    detail: &'static str,
    affected_pipelines: Vec<String>,
}

fn provider_kind(capability: ProviderCapability) -> ProviderKind {
    match capability {
        ProviderCapability::Stt => ProviderKind::Stt,
        ProviderCapability::Llm => ProviderKind::Llm,
        ProviderCapability::Tts => ProviderKind::Tts,
        ProviderCapability::Tool => ProviderKind::Tool,
    }
}

fn validate_provider_definition(definition: &ProviderDefinition) -> Result<(), ApiError> {
    match &definition.variant {
        ProviderDefinitionVariant::OpenAiLlm { base_url, .. }
        | ProviderDefinitionVariant::OpenAiStt { base_url, .. }
        | ProviderDefinitionVariant::OpenAiTts { base_url, .. } => {
            validate_http_url("base_url", base_url)?;
        }
        ProviderDefinitionVariant::WyomingStt { .. }
        | ProviderDefinitionVariant::WyomingTts { .. }
        | ProviderDefinitionVariant::McpTool { .. } => {}
    }
    Ok(())
}

fn validate_http_url(field: &str, value: &str) -> Result<(), ApiError> {
    let uri = value.parse::<Uri>().map_err(|error| {
        ApiError::unprocessable(format!("{field} is not a valid URL: {error}"))
    })?;
    if uri.host().is_none() {
        return Err(ApiError::unprocessable(format!("{field} must include a host")));
    }
    let Some(scheme) = uri.scheme_str() else {
        return Err(ApiError::unprocessable(format!("{field} must include a URL scheme")));
    };
    if !matches!(scheme, "http" | "https") {
        return Err(ApiError::unprocessable(format!(
            "{field} must use http or https, got `{}`",
            scheme
        )));
    }
    Ok(())
}

fn status_from_health(
    kind: ProviderKind,
    id: String,
    health: Health,
    affects_pipelines: Vec<String>,
) -> ProviderStatus {
    let reachable = health.is_usable();
    let (state, message) = match health {
        Health::Healthy => (ProviderStatusState::Reachable, None),
        Health::Degraded { reason } => (ProviderStatusState::Reachable, Some(reason)),
        Health::Unhealthy { reason } => (ProviderStatusState::Configured, Some(reason)),
    };
    ProviderStatus {
        id,
        kind,
        state,
        configured: true,
        reachable,
        proven_by_turn: None,
        message,
        affects_pipelines,
    }
}

fn unregistered_status(
    kind: ProviderKind,
    id: String,
    affects_pipelines: Vec<String>,
) -> ProviderStatus {
    ProviderStatus {
        id: id.clone(),
        kind,
        state: ProviderStatusState::Unavailable,
        configured: true,
        reachable: false,
        proven_by_turn: None,
        message: Some(format!(
            "provider definition `{id}` is not registered in the runtime provider snapshot"
        )),
        affects_pipelines,
    }
}

async fn affected_pipelines(
    state: &AppState,
    provider_id: &str,
) -> Result<Vec<String>, ApiError> {
    let mut affected = Vec::new();
    for name in state.pipeline_names().await.map_err(store_failure)? {
        let Some(graph) = state.pipeline(&name).await.map_err(store_failure)? else {
            continue;
        };
        if graph.nodes.iter().any(|node| node.provider == provider_id) {
            affected.push(name);
        }
    }
    Ok(affected)
}

fn store_failure(error: conduit_core::Error) -> ApiError {
    match error {
        conduit_core::Error::Config(detail) => ApiError::unprocessable(detail),
        other => ApiError::unavailable(other.to_string()),
    }
}
