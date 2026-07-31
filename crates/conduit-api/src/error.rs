//! HTTP error representation.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// An error rendered as a JSON body with an appropriate status.
///
/// Every failure the API returns goes through here, so clients can rely on a
/// single error shape: `{"error": "...", "detail": "..."}`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    kind: &'static str,
    detail: String,
}

impl ApiError {
    /// The requested resource does not exist.
    #[must_use]
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_FOUND, kind: "not_found", detail: detail.into() }
    }

    /// The store could not answer.
    #[must_use]
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            kind: "unavailable",
            detail: detail.into(),
        }
    }

    /// The request was well-formed JSON but semantically invalid.
    #[must_use]
    pub fn unprocessable(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            kind: "invalid",
            detail: detail.into(),
        }
    }
}

#[derive(Serialize)]
struct Body<'a> {
    error: &'a str,
    detail: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(Body { error: self.kind, detail: &self.detail })).into_response()
    }
}
