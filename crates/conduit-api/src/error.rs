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

    /// No usable credential was presented.
    ///
    /// The detail is deliberately the same whether the header was missing,
    /// malformed, or carried a token nobody issued: it tells a misconfigured
    /// client what shape to send, and tells someone guessing tokens nothing
    /// about which ones exist.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            kind: "unauthorized",
            detail: "expected an `Authorization: Bearer <token>` header".to_owned(),
        }
    }

    /// The credential was recognised but is not allowed to do this.
    ///
    /// Unlike [`ApiError::unauthorized`] this can afford to be specific: the
    /// caller has already proved who they are, so naming what they were denied
    /// leaks nothing and is the difference between a five-minute fix and an
    /// afternoon.
    #[must_use]
    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self { status: StatusCode::FORBIDDEN, kind: "forbidden", detail: detail.into() }
    }
}

#[derive(Serialize)]
struct Body<'a> {
    error: &'a str,
    detail: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(Body { error: self.kind, detail: &self.detail });
        if self.status == StatusCode::UNAUTHORIZED {
            // Standard HTTP tooling looks here to learn which scheme to use,
            // and the RFC requires it on a 401.
            return (self.status, [(axum::http::header::WWW_AUTHENTICATE, "Bearer")], body)
                .into_response();
        }
        (self.status, body).into_response()
    }
}
