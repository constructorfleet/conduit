//! Provider definition endpoints.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use conduit_provider::storage::{ProviderCapability, ProviderDefinition};
use serde::Serialize;

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::pipelines::{component_catalog, PipelineComponentCatalog};
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

#[derive(Serialize)]
struct DeleteConflict {
    error: &'static str,
    detail: &'static str,
    affected_pipelines: Vec<String>,
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
