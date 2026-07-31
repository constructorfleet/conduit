//! HTTP surface for Conduit.
//!
//! Two things live here and nothing else: CRUD over pipeline definitions, and
//! a live view of the event bus. Anything that processes audio belongs in the
//! runtime, not in the API.

pub mod config;
pub mod converse;
pub mod error;
pub mod events;
pub mod pipelines;
pub mod state;

use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

pub use error::ApiError;
pub use state::AppState;

/// Builds the application router.
///
/// Kept separate from serving so tests can drive it directly without binding
/// a port.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/v1/events", get(events::stream))
        .route("/v1/pipelines", get(pipelines::list))
        .route("/v1/pipelines/validate", post(pipelines::validate))
        .route("/v1/pipelines/{name}/converse", get(converse::converse))
        .route(
            "/v1/pipelines/{name}",
            get(pipelines::get).put(pipelines::put).delete(pipelines::delete),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Prometheus scrape endpoint.
///
/// Served as plain text with the version Prometheus expects, so a scraper
/// needs no special configuration.
async fn metrics(axum::extract::State(state): axum::extract::State<AppState>) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics().render(),
    )
        .into_response()
}

/// Liveness and version probe.
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
