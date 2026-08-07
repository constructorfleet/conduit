//! HTTP surface for Conduit.
//!
//! Two things live here and nothing else: CRUD over pipeline definitions, and
//! a live view of the event bus. Anything that processes audio belongs in the
//! runtime, not in the API.
//!
//! There are two listeners, built by [`router`] and [`ops_router`]. The service
//! router carries everything that touches conversations or configuration and
//! authenticates every route; the ops router carries `/health`, `/ready`, and
//! `/metrics` and authenticates nothing. That split is deliberate: probes
//! cannot present a credential, and a scrape that needs one silently stops
//! working when the token changes. What protects the ops listener is where it
//! is bound and what the firewall publishes, not a token.

pub mod auth;
pub mod compare;
pub mod config;
pub mod converse;
pub mod error;
pub mod esphome;
pub mod events;
pub mod factory;
pub mod firmware;
pub mod pipelines;
pub mod providers;
pub mod speakers;
pub mod state;
pub mod status;
pub mod turns;

use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

pub use error::ApiError;
pub use state::AppState;

/// Maximum request body accepted by the service router.
pub const REQUEST_BODY_LIMIT_BYTES: usize = 1024 * 1024;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The route a device opens to hold a conversation.
///
/// Named rather than written inline because firmware builds the same path from
/// its own constants, and the parity check between them needs something to
/// compare against.
pub const CONVERSE_ROUTE: &str = "/v1/pipelines/{name}/converse";

/// Builds the service router: conversations and configuration.
///
/// Every route here requires a bearer token, enforced by the handlers'
/// extractors rather than by a middleware, so a route added without thinking
/// about who may call it does not compile.
///
/// Kept separate from serving so tests can drive it directly without binding
/// a port.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/events", get(events::stream))
        .route("/v1/status", get(status::get))
        .route("/v1/turns", get(turns::list))
        .route("/v1/turns/live", get(turns::live))
        .route("/v1/turns/{turn_id}", get(turns::get))
        .route("/v1/turns/{turn_id}/events", get(turns::events))
        .route("/v1/catalog/providers", get(providers::catalog))
        .route("/v1/providers", get(providers::list))
        .route(
            "/v1/providers/{id}",
            get(providers::get).put(providers::put).delete(providers::delete),
        )
        .route("/v1/providers/{id}/rename", post(providers::rename))
        .route("/v1/providers/{id}/voices", get(providers::voices))
        .route("/v1/providers/{id}/phrases", get(providers::phrases))
        .route("/v1/providers/{id}/test", post(providers::test))
        .route("/v1/speakers", get(speakers::list).post(speakers::create))
        .route(
            "/v1/speakers/{id}",
            get(speakers::get).put(speakers::rename).delete(speakers::delete),
        )
        .route(
            "/v1/speakers/{id}/enroll",
            post(speakers::enroll)
                // Its own budget: enrollment is the one route that carries
                // audio, and the service-wide limit below would cut a
                // recording short.
                .layer(DefaultBodyLimit::max(speakers::ENROLLMENT_BODY_LIMIT_BYTES)),
        )
        .route("/v1/devices/{device}/firmware", get(firmware::render))
        .route("/v1/devices/{device}/firmware/flash", post(firmware::flash))
        .route("/v1/pipelines", get(pipelines::list))
        .route("/v1/pipelines/validate", post(pipelines::validate))
        .route("/v1/pipelines/{name}/test-turn", post(pipelines::test_turn))
        .route(
            "/v1/pipelines/compare",
            post(compare::compare)
                // Its own budget, for the same reason enrollment has one: a
                // comparison may carry a recorded fixture, and the service-wide
                // limit is sized for JSON.
                .layer(DefaultBodyLimit::max(compare::COMPARISON_BODY_LIMIT_BYTES)),
        )
        .route(CONVERSE_ROUTE, get(converse::converse))
        .route(
            "/v1/pipelines/{name}",
            get(pipelines::get).put(pipelines::put).delete(pipelines::delete),
        )
        .layer(DefaultBodyLimit::max(REQUEST_BODY_LIMIT_BYTES))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, REQUEST_TIMEOUT))
        // `include_headers(false)` is the default, and stated anyway: spans go
        // to an OTLP collector, and a span carrying `Authorization` would ship
        // every device's credential to it. Keep this outside the protective
        // layers so every request still gets a request span.
        .layer(
            TraceLayer::new_for_http().make_span_with(
                tower_http::trace::DefaultMakeSpan::new().include_headers(false),
            ),
        )
        .with_state(state)
}

/// Builds the ops router: `/health`, `/ready`, and `/metrics`,
/// unauthenticated.
///
/// Bound to its own port so an operator can publish the service port to the
/// network and keep this one on the host. It exposes real operational
/// intelligence — conversation counts, tool names, error rates — so it should
/// not cross a trust boundary.
pub fn ops_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
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

/// Readiness probe that verifies the pipeline store can answer.
async fn ready(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    state.pipeline_names().await.map_err(|error| {
        ApiError::unavailable(format!("pipeline store is not ready: {error}"))
    })?;

    Ok(axum::Json(serde_json::json!({
        "status": "ready",
        "version": env!("CARGO_PKG_VERSION"),
    })))
}
