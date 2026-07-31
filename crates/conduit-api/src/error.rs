//! HTTP error representation.

use axum::body::Body as AxumBody;
use axum::extract::rejection::JsonRejection;
use axum::extract::FromRequest;
use axum::http::StatusCode;
use axum::http::{header, Request};
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

    /// The request body or parameters could not be parsed.
    #[must_use]
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, kind: "bad_request", detail: detail.into() }
    }

    /// The request body exceeds the configured API limit.
    #[must_use]
    pub fn payload_too_large(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            kind: "payload_too_large",
            detail: detail.into(),
        }
    }

    /// The request content type is not one this endpoint accepts.
    #[must_use]
    pub fn unsupported_media_type(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            kind: "unsupported_media_type",
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

    /// Converts axum's JSON extractor failures into the API's stable JSON
    /// error envelope.
    #[must_use]
    pub fn from_json_rejection(rejection: JsonRejection) -> Self {
        let detail = rejection.body_text();
        match rejection.status() {
            StatusCode::PAYLOAD_TOO_LARGE => Self::payload_too_large(detail),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::unsupported_media_type(detail),
            _ => Self::bad_request(detail),
        }
    }
}

/// JSON request extractor that reports failures with [`ApiError`].
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    S: Send + Sync,
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
{
    type Rejection = ApiError;

    async fn from_request(
        request: Request<AxumBody>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(ApiError::from_json_rejection)
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
            return (self.status, [(header::WWW_AUTHENTICATE, "Bearer")], body).into_response();
        }
        (self.status, body).into_response()
    }
}
