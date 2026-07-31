//! Pipeline definition endpoints.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use conduit_core::graph::PipelineGraph;
use serde::Serialize;

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
pub async fn list(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.pipeline_names())
}

/// `GET /v1/pipelines/{name}` — one pipeline with its execution order.
///
/// # Errors
///
/// Returns 404 if no pipeline is stored under `name`.
pub async fn get(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<PipelineView>, ApiError> {
    let graph = state
        .pipeline(&name)
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
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(graph): Json<PipelineGraph>,
) -> Result<(StatusCode, Json<PipelineView>), ApiError> {
    let view = view(graph.clone())?;
    let replaced = state.put_pipeline(name, graph);
    let status = if replaced { StatusCode::OK } else { StatusCode::CREATED };
    Ok((status, Json(view)))
}

/// `DELETE /v1/pipelines/{name}` — removes a pipeline.
///
/// # Errors
///
/// Returns 404 if no pipeline is stored under `name`.
pub async fn delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.remove_pipeline(&name) {
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
