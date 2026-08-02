//! Who may call the service API, and who may not.
//!
//! These drive the real routers, because the policy is only true if it is true
//! of a request. Each test reads as the rule it enforces: a device token cannot
//! read events; an unknown token learns nothing from being refused.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::auth::{Access, Tokens};
use conduit_api::{ops_router, router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Node, PipelineGraph};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Long enough to pass the entropy floor. The value is irrelevant; only that
/// the right caller presents the right one.
const DEVICE_TOKEN: &str = "device-token-000000000000000000000000";
const RESTRICTED_TOKEN: &str = "restricted-token-00000000000000000000";
const MANAGEMENT_TOKEN: &str = "management-token-00000000000000000000";

fn token_file() -> String {
    format!(
        r#"{{
          "devices": [
            {{ "token": "{DEVICE_TOKEN}", "device": "kitchen" }},
            {{ "token": "{RESTRICTED_TOKEN}", "device": "guest",
               "pipelines": ["guest-room"] }}
          ],
          "management": [
            {{ "token": "{MANAGEMENT_TOKEN}", "name": "ui" }}
          ]
        }}"#
    )
}

/// State that authenticates against [`token_file`].
fn guarded() -> AppState {
    let tokens = Tokens::parse(&token_file()).expect("the token file parses");
    AppState::new(EventBus::default()).with_access(Access::Tokens(tokens))
}

fn valid_graph() -> PipelineGraph {
    PipelineGraph::new("kitchen")
        .with_node(Node::stt("stt", "whisper"))
        .with_node(Node::core("core", "ollama"))
        .with_node(Node::tts("tts", "piper"))
        .with_edge(Edge::new("stt", "core"))
        .with_edge(Edge::new("core", "tts"))
}

/// Sends `request` to the service router.
async fn call(
    state: &AppState,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, headers, json)
}

/// A GET carrying no credential.
fn anonymous(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
}

/// A GET carrying `authorization` verbatim, so malformed values can be tested.
fn with_header(uri: &str, authorization: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", authorization)
        .body(Body::empty())
        .expect("request")
}

/// A GET carrying `token` as a bearer credential.
fn as_bearer(uri: &str, token: &str) -> Request<Body> {
    with_header(uri, &format!("Bearer {token}"))
}

#[tokio::test]
async fn managing_pipelines_without_a_token_is_refused() {
    let state = guarded();
    for uri in ["/v1/pipelines", "/v1/pipelines/kitchen", "/v1/events"] {
        let (status, _, body) = call(&state, anonymous(uri)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must need a token: {body}");
    }
}

#[tokio::test]
async fn deleting_a_pipeline_without_a_token_is_refused() {
    // The route someone would most regret leaving open.
    let state = guarded();
    let request = Request::builder()
        .method("DELETE")
        .uri("/v1/pipelines/kitchen")
        .body(Body::empty())
        .expect("request");
    let (status, _, _) = call(&state, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn storing_a_pipeline_without_a_token_is_refused() {
    let state = guarded();
    let request = Request::builder()
        .method("PUT")
        .uri("/v1/pipelines/kitchen")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&valid_graph()).expect("serialize")))
        .expect("request");
    let (status, _, _) = call(&state, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn validating_a_graph_without_a_token_is_refused() {
    // Cheap to call and it reveals what the server can run, so it is not a
    // reasonable thing to leave open either.
    let state = guarded();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/pipelines/validate")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&valid_graph()).expect("serialize")))
        .expect("request");
    let (status, _, _) = call(&state, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_management_token_may_manage_pipelines() {
    let state = guarded();
    let (status, _, body) = call(&state, as_bearer("/v1/pipelines", MANAGEMENT_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_device_token_may_not_read_events() {
    // The load-bearing rule: a token lifted from a satellite must not become a
    // way to read everything said in the house.
    let state = guarded();
    let (status, _, body) = call(&state, as_bearer("/v1/events", DEVICE_TOKEN)).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
async fn a_device_token_may_not_manage_pipelines() {
    let state = guarded();
    let request = Request::builder()
        .method("DELETE")
        .uri("/v1/pipelines/kitchen")
        .header("authorization", format!("Bearer {DEVICE_TOKEN}"))
        .body(Body::empty())
        .expect("request");
    let (status, _, body) = call(&state, request).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn an_unknown_token_is_refused_exactly_like_a_missing_one() {
    // Anything that told them apart would let someone probe for valid tokens.
    let state = guarded();
    let (missing_status, _, missing) = call(&state, anonymous("/v1/pipelines")).await;
    let (unknown_status, _, unknown) =
        call(&state, as_bearer("/v1/pipelines", "nobody-holds-this-token-000000000000")).await;

    assert_eq!(missing_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown, missing, "an unknown token must reveal nothing a missing one does not");
}

#[tokio::test]
async fn a_malformed_authorization_header_is_refused() {
    let state = guarded();
    for header in [
        "",
        "Bearer",
        "Bearer ",
        MANAGEMENT_TOKEN,
        &format!("Basic {MANAGEMENT_TOKEN}"),
        &format!("Bearer  {MANAGEMENT_TOKEN} extra"),
    ] {
        let (status, _, _) = call(&state, with_header("/v1/pipelines", header)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "`{header}` is not a usable credential");
    }
}

#[tokio::test]
async fn the_scheme_is_matched_case_insensitively() {
    // RFC 7235 says the scheme is case-insensitive, and a client that sends
    // `bearer` is not the problem worth failing on.
    let state = guarded();
    let (status, _, body) =
        call(&state, with_header("/v1/pipelines", &format!("bearer {MANAGEMENT_TOKEN}"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_401_says_which_scheme_to_use() {
    let state = guarded();
    let (status, headers, _) = call(&state, anonymous("/v1/pipelines")).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers.get("www-authenticate").and_then(|value| value.to_str().ok()),
        Some("Bearer"),
        "standard HTTP tooling looks here"
    );
}

#[tokio::test]
async fn an_auth_failure_uses_the_same_error_shape_as_everything_else() {
    // A client's error handling should need no special case for auth.
    let state = guarded();
    let (_, _, body) = call(&state, anonymous("/v1/pipelines")).await;

    assert_eq!(body["error"], "unauthorized");
    assert!(
        body["detail"].as_str().is_some_and(|detail| detail.contains("Authorization")),
        "a misconfigured client deserves to be told the format: {body}"
    );
}

#[tokio::test]
async fn a_token_is_never_reflected_back_to_the_caller() {
    // Error bodies end up in client logs and bug reports.
    let state = guarded();
    let (_, _, body) = call(&state, as_bearer("/v1/events", DEVICE_TOKEN)).await;
    let rendered = body.to_string();
    assert!(!rendered.contains(DEVICE_TOKEN), "the response quoted the token: {rendered}");
}

#[tokio::test]
async fn a_query_parameter_is_not_a_credential() {
    // Deliberately unsupported: the firmware logs whole URLs and the trace layer
    // records request URIs into exportable spans, so a token in a URL leaks.
    let state = guarded();
    let (status, _, _) =
        call(&state, anonymous(&format!("/v1/pipelines?token={MANAGEMENT_TOKEN}"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) =
        call(&state, anonymous(&format!("/v1/pipelines?access_token={MANAGEMENT_TOKEN}")))
            .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_ops_router_needs_no_credential() {
    // A liveness probe cannot present one, and a scrape that needed one would
    // silently break whenever the token changed.
    let state = guarded();
    for uri in ["/health", "/metrics"] {
        let response =
            ops_router(state.clone()).oneshot(anonymous(uri)).await.expect("responds");
        assert_eq!(response.status(), StatusCode::OK, "{uri} must not need a token");
    }
}

#[tokio::test]
async fn the_service_router_does_not_serve_the_ops_routes() {
    // They belong to the other listener. Serving them here too would mean the
    // published port exposes metrics however the ops port is firewalled.
    let state = guarded();
    for uri in ["/health", "/metrics"] {
        let (status, _, _) = call(&state, anonymous(uri)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must not be on the service port");
    }
}

#[tokio::test]
async fn the_ops_router_does_not_serve_the_service_routes() {
    // Otherwise the unauthenticated port would be a way around the tokens.
    let state = guarded();
    for uri in ["/v1/pipelines", "/v1/events"] {
        let response =
            ops_router(state.clone()).oneshot(anonymous(uri)).await.expect("responds");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{uri} must not be reachable without a token"
        );
    }
}

#[tokio::test]
async fn an_open_server_asks_for_nothing() {
    // What `CONDUIT_ALLOW_ANONYMOUS` buys, and the default for a bare AppState
    // so that a library caller is not forced to invent tokens.
    let state = AppState::new(EventBus::default()).with_access(Access::anonymous());
    let (status, _, body) = call(&state, anonymous("/v1/pipelines")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A token file that cleans itself up.
///
/// The only test here that needs a real file: every other rule is checked
/// against [`Tokens::parse`], which takes a string.
struct TokenFile(std::path::PathBuf);

impl TokenFile {
    fn new(label: &str, mode: u32) -> Self {
        let path = std::env::temp_dir().join(format!(
            "conduit-tokens-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::write(&path, token_file()).expect("writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .expect("sets permissions");
        }
        Self(path)
    }
}

impl Drop for TokenFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
#[cfg(unix)]
async fn a_token_file_other_users_can_read_stops_the_server() {
    // Tokens are plaintext, so the permissions *are* the protection. Catching
    // this at startup turns a silent exposure into a failure someone fixes.
    let file = TokenFile::new("loose", 0o644);
    let error = Tokens::load(&file.0).await.expect_err("a readable token file is refused");

    let message = error.to_string();
    assert!(message.contains("readable by other users"), "{message}");
    assert!(message.contains("chmod"), "the operator needs telling how to fix it: {message}");
}

#[tokio::test]
#[cfg(unix)]
async fn a_group_readable_token_file_stops_the_server() {
    // Group-readable is the mistake a deployment tool makes, and it is just as
    // much of an exposure as world-readable.
    let file = TokenFile::new("group", 0o640);
    let error = Tokens::load(&file.0).await.expect_err("a group-readable file is refused");
    assert!(error.to_string().contains("readable by other users"), "{error}");
}

#[tokio::test]
#[cfg(unix)]
async fn a_private_token_file_loads() {
    let file = TokenFile::new("private", 0o600);
    let tokens = Tokens::load(&file.0).await.expect("a private token file loads");
    assert_eq!(tokens.len(), 3);
}

#[tokio::test]
async fn a_missing_token_file_stops_the_server() {
    // Naming the path, because a typo is the likeliest cause.
    let missing = std::env::temp_dir().join("conduit-tokens-does-not-exist");
    let error = Tokens::load(&missing).await.expect_err("a missing token file is refused");
    assert!(error.to_string().contains("cannot read the token file"), "{error}");
}

// What the service *writes* about a request carrying a token is checked in
// `token_logging.rs`, which needs a process to itself to install a subscriber.
