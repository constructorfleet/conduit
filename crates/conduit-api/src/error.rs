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

    /// The route exists but this deployment has not configured what it needs.
    ///
    /// Not 404, which would say the route is wrong, and not 422, which would
    /// blame the request: the request is fine and the server has nothing to
    /// carry it out with until an operator configures one.
    #[must_use]
    pub fn not_implemented(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            kind: "not_configured",
            detail: detail.into(),
        }
    }

    /// A service Conduit does not own could not be reached, or refused.
    ///
    /// Distinct from [`ApiError::unavailable`], which says *this* server cannot
    /// answer. 502 says Conduit is fine and something it was asked to talk to is
    /// not, which is a different next move for the operator: check that service,
    /// not this one.
    #[must_use]
    pub fn bad_gateway(detail: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_GATEWAY, kind: "bad_gateway", detail: detail.into() }
    }

    /// The request cannot be applied to the resource as it currently stands.
    ///
    /// Distinct from [`ApiError::unprocessable`]: the request is one the API
    /// would accept, and would accept again once whatever is in the way is
    /// gone. Retrying it unchanged is meaningful, which is why the status is
    /// not 422.
    #[must_use]
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self { status: StatusCode::CONFLICT, kind: "conflict", detail: detail.into() }
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
        // Without this an operator watching the logs sees a clean server while
        // the console shows a bare status code, and the detail explaining the
        // refusal exists only in a response body nobody kept. Client mistakes
        // are expected traffic, so they warn; a failure the server owns is a
        // problem with the server.
        //
        // A 401 is logged without its detail. That detail is the fixed string
        // naming the `Authorization` header, so it describes nothing about the
        // request that `kind` does not — and `token_logging.rs` asserts the
        // header is never named in anything shipped to a log collector, which
        // is a line worth keeping bright rather than carving an exception into.
        let detail = if self.status == StatusCode::UNAUTHORIZED { "" } else { &self.detail };
        if self.status.is_server_error() {
            tracing::error!(status = %self.status, kind = self.kind, detail, "request failed");
        } else {
            tracing::warn!(status = %self.status, kind = self.kind, detail, "request rejected");
        }
        let body = Json(Body { error: self.kind, detail: &self.detail });
        if self.status == StatusCode::UNAUTHORIZED {
            // Standard HTTP tooling looks here to learn which scheme to use,
            // and the RFC requires it on a 401.
            return (self.status, [(header::WWW_AUTHENTICATE, "Bearer")], body).into_response();
        }
        (self.status, body).into_response()
    }
}
