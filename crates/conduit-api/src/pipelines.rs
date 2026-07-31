//! Pipeline definition endpoints.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use conduit_core::graph::PipelineGraph;
use serde::Serialize;

use crate::auth::ManagementCaller;
use crate::{ApiError, AppState};

/// A stored pipeline and its execution order.
#[derive(Debug, Serialize)]
pub struct PipelineView {
    /// The stored definition.
    pub graph: PipelineGraph,
    /// Node ids in execution order, as the runtime would walk them.
    pub order: Vec<String>,
}

/// `GET /v1/pipelines` — names of every stored pipeline.
///
/// # Errors
///
/// Returns 503 if the store cannot be read.
pub async fn list(
    _caller: ManagementCaller,
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, ApiError> {
    state.pipeline_names().await.map(Json).map_err(store_failure)
}

/// Turns a store failure into a response.
///
/// A name the store refuses is the client's mistake; anything else is the
/// server's, and the status has to say which.
fn store_failure(error: conduit_core::Error) -> ApiError {
    match error {
        conduit_core::Error::Config(detail) => ApiError::unprocessable(detail),
        other => ApiError::unavailable(other.to_string()),
    }
}

/// `GET /v1/pipelines/{name}` — one pipeline with its execution order.
///
/// # Errors
///
/// Returns 404 if no pipeline is stored under `name`, or 503 if the store
/// cannot be read.
pub async fn get(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PipelineView>, ApiError> {
    let graph = state
        .pipeline(&name)
        .await
        .map_err(store_failure)?
        .ok_or_else(|| ApiError::not_found(format!("no pipeline named `{name}`")))?;
    Ok(Json(view(graph)?))
}

/// `PUT /v1/pipelines/{name}` — stores a pipeline after validating it.
///
/// Returns 201 for a new pipeline and 200 when replacing one. Invalid graphs
/// are rejected rather than stored, so anything readable back is executable.
///
/// # Errors
///
/// Returns 422 if the graph fails validation.
pub async fn put(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(graph): Json<PipelineGraph>,
) -> Result<(StatusCode, Json<PipelineView>), ApiError> {
    let view = view(graph.clone())?;
    let replaced = state.put_pipeline(&name, graph).await.map_err(store_failure)?;
    let status = if replaced { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(view)))
}

/// `DELETE /v1/pipelines/{name}` — removes a pipeline.
///
/// # Errors
///
/// Returns 404 if no pipeline is stored under `name`.
pub async fn delete(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.remove_pipeline(&name).await.map_err(store_failure)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no pipeline named `{name}`")))
    }
}

/// `POST /v1/pipelines/validate` — checks a graph without storing it.
///
/// This is what the graph editor calls on every edit.
///
/// # Errors
///
/// Returns 422 if the graph fails validation.
pub async fn validate(
    _caller: ManagementCaller,
    Json(graph): Json<PipelineGraph>,
) -> Result<Json<PipelineView>, ApiError> {
    view(graph).map(Json)
}

/// Validates `graph` and pairs it with its execution order.
fn view(graph: PipelineGraph) -> Result<PipelineView, ApiError> {
    let order = graph
        .topological_order()
        .map_err(|error| ApiError::unprocessable(error.to_string()))?
        .iter()
        .map(|node| node.id.clone())
        .collect();
    Ok(PipelineView { graph, order })
}
