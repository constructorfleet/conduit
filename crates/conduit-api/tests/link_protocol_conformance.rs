//! Link protocol conformance suite (spec 0005).
//!
//! This file is the seam every Link-protocol change is measured against:
//! Conduit passes as a host, a mock peer passes as a client. The peer is a
//! real in-process axum server on a TCP listener; Conduit's proxy reaches it
//! over its own reqwest client, so every proxy assertion here is a real
//! network roundtrip — no internal HTTP mocking.
//!
//! Matrix pinned (issue #155):
//!  1. `POST /v1/linked-services` mints a sync token and hashes it server-side.
//!  2. `GET  /v1/linked-services` lists with resolved panel.
//!  3. Panel resolution: (a) explicit manifest wins, (b) typed-kind fallback
//!     still returns a panel for pre-manifest rows, (c) `Generic` with no
//!     panel is filtered out of the list.
//!  4. Proxy `GET` / `POST` (with body) / streamed body response.
//!  5. `Location` rewrite: absolute-path, same-origin absolute, cross-origin
//!     passthrough.
//!  6. Header stripping (authorization, host, content-length, connection) on
//!     both request and response.
//!  7. Operator-authenticated `DELETE`.
//!  8. Peer-authenticated revoke with `Authorization: Bearer {sync_token}`;
//!     wrong token → 401.
//!  9. Replay resistance: a leaked SHA-256 hash cannot be used as a bearer.
//!
//! Duplicates in `linked_services.rs` and `vox_proxy.rs` are intentionally left
//! in place where they exercise Vox-specific auto-provision, event publishing,
//! or extension-validation logic that this file does not repeat.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::Router;
use conduit_api::linked_services::hash_token;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_link::{LinkedServiceKind, LinkedServicePanel};
use conduit_provider::storage::LinkedService;
use futures_util::stream;
use http_body_util::BodyExt;
use tokio::sync::Mutex;
use tower::ServiceExt;

const OPERATOR_TOKEN: &str = "Bearer test-management-token-abcdefghijklmnopqrstuvwxyz";

// ── harness ────────────────────────────────────────────────────────────────

async fn call(state: &AppState, request: Request<Body>) -> axum::response::Response {
    router(state.clone()).oneshot(request).await.expect("router responds")
}

async fn json_response(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, json)
}

fn operator_get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", OPERATOR_TOKEN)
        .body(Body::empty())
        .expect("request")
}

fn operator_delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", OPERATOR_TOKEN)
        .body(Body::empty())
        .expect("request")
}

fn operator_post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", OPERATOR_TOKEN)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request")
}

fn memoria_link_body(peer_base_url: &str) -> serde_json::Value {
    serde_json::json!({
        "service_kind": "memoria",
        "peer_name": "Household Memory",
        "peer_id": "household-memory",
        "peer_base_url": peer_base_url,
        "panel": {
            "id": "memoria",
            "label": "Memoria",
            "icon": "brain",
            "path": "/ui/",
        }
    })
}

// ── mock peer ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct ObservedRequest {
    method: String,
    path_and_query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct PeerState {
    /// Last request that hit `/echo` / any observed route.
    seen: Arc<Mutex<Option<ObservedRequest>>>,
    /// Base URL of this peer, filled in after bind so response handlers can
    /// build same-origin `Location` headers with a real host+port.
    self_base: Arc<Mutex<String>>,
}

async fn observe(state: &PeerState, request: Request<Body>) -> Vec<u8> {
    let (parts, body) = request.into_parts();
    let bytes = body.collect().await.expect("body").to_bytes().to_vec();
    let headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            (name.as_str().to_owned(), value.to_str().unwrap_or("").to_owned())
        })
        .collect();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.path().to_owned(), |value| value.as_str().to_owned());
    *state.seen.lock().await = Some(ObservedRequest {
        method: parts.method.to_string(),
        path_and_query,
        headers,
        body: bytes.clone(),
    });
    bytes
}

struct Peer {
    base_url: String,
    state: PeerState,
}

/// Spins a real axum listener on 127.0.0.1:0 exposing the service-side
/// surface Conduit's proxy actually calls (spec 0005 §Reverse-proxy contract):
/// `/link/health`, a `/ui/*` echo (observed for header + body assertions),
/// and a handful of redirect emitters used by the Location-rewrite cases.
async fn spawn_peer() -> Peer {
    let state = PeerState::default();

    async fn health() -> StatusCode {
        StatusCode::OK
    }

    async fn echo(State(peer): State<PeerState>, request: Request<Body>) -> impl IntoResponse {
        let bytes = observe(&peer, request).await;
        // Return the observed body verbatim + a hop-by-hop response header
        // Conduit MUST drop, plus a benign header it MUST forward, so both
        // sides of §Reverse-proxy contract are exercised.
        let mut response = axum::response::Response::new(Body::from(bytes));
        let headers = response.headers_mut();
        headers.insert("content-type", HeaderValue::from_static("application/octet-stream"));
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        headers.insert("x-peer-extra", HeaderValue::from_static("forwarded"));
        response
    }

    async fn stream_body() -> impl IntoResponse {
        // Multi-chunk stream. Conduit forwards without buffering, so the
        // client sees a single collected body assembled from N chunks.
        let chunks: Vec<Result<axum::body::Bytes, Infallible>> = vec![
            Ok(axum::body::Bytes::from_static(b"chunk-1|")),
            Ok(axum::body::Bytes::from_static(b"chunk-2|")),
            Ok(axum::body::Bytes::from_static(b"chunk-3")),
        ];
        let body = Body::from_stream(stream::iter(chunks));
        let mut response = axum::response::Response::new(body);
        response.headers_mut().insert("content-type", HeaderValue::from_static("text/plain"));
        response
    }

    async fn redirect_absolute_path() -> impl IntoResponse {
        (StatusCode::TEMPORARY_REDIRECT, [("location", "/ui?from=redirect")])
    }

    async fn redirect_same_origin(State(peer): State<PeerState>) -> impl IntoResponse {
        let base = peer.self_base.lock().await.clone();
        let location = format!("{base}/ui/authorised");
        (StatusCode::TEMPORARY_REDIRECT, [("location", location)])
    }

    async fn redirect_cross_origin() -> impl IntoResponse {
        (StatusCode::TEMPORARY_REDIRECT, [("location", "https://cross.example.test/foreign")])
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let address = listener.local_addr().expect("listener address");
    let base_url = format!("http://{address}");
    *state.self_base.lock().await = base_url.clone();

    let app = Router::new()
        .route("/link/health", get(health))
        .route("/redirect/absolute-path", get(redirect_absolute_path))
        .route("/redirect/same-origin", get(redirect_same_origin))
        .route("/redirect/cross-origin", get(redirect_cross_origin))
        .route("/stream", get(stream_body))
        .route("/echo", any(echo))
        .route("/echo/{*rest}", any(echo))
        .with_state(state.clone());

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer serves");
    });

    Peer { base_url, state }
}

async fn seed_link(state: &AppState, peer_id: &str, peer_base_url: &str) -> String {
    let sync_token = "sync-token-real";
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Memoria,
            peer_id: peer_id.to_owned(),
            peer_name: "Household Memory".to_owned(),
            peer_base_url: peer_base_url.to_owned(),
            sync_token_hash: hash_token(sync_token),
            provider_definition_id: String::new(),
            panel: Some(LinkedServicePanel {
                id: "memoria".to_owned(),
                label: "Memoria".to_owned(),
                icon: "brain".to_owned(),
                path: "/ui/".to_owned(),
            }),
            granted_by: "operator".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
            reachability: conduit_link::Reachability::Unknown,
            last_probed_at: None,
        })
        .await
        .expect("stores");
    sync_token.to_owned()
}

// ── 1 & 2 & 3a: create + list with explicit manifest ──────────────────────

#[tokio::test]
async fn post_creates_row_returns_sync_token_hashed_server_side() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());

    let (status, body) = json_response(
        call(
            &state,
            operator_post_json("/v1/linked-services", memoria_link_body(&peer.base_url)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let sync_token = body["sync_token"].as_str().expect("sync_token").to_owned();
    assert!(!sync_token.is_empty(), "sync token cannot be empty");

    let stored =
        state.linked_service("household-memory").await.expect("store").expect("stored row");
    assert_eq!(
        stored.sync_token_hash,
        hash_token(&sync_token),
        "server MUST store the SHA-256 hash, not the raw token"
    );
    assert_ne!(
        stored.sync_token_hash, sync_token,
        "the raw token MUST never appear in the stored row"
    );
}

#[tokio::test]
async fn get_lists_with_explicit_manifest_panel_winning() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    let mut body = memoria_link_body(&peer.base_url);
    body["panel"]["label"] = serde_json::json!("Explicit Wins");

    call(&state, operator_post_json("/v1/linked-services", body)).await;

    let (status, list) =
        json_response(call(&state, operator_get("/v1/linked-services")).await).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let rows = list.as_array().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["panel"]["label"], "Explicit Wins");
    assert_eq!(rows[0]["panel"]["path"], "/ui/");
}

// ── 3b: typed-kind fallback panel for a pre-manifest row ──────────────────

#[tokio::test]
async fn typed_kind_fallback_synthesises_a_panel_for_pre_manifest_rows() {
    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Memoria,
            peer_id: "legacy-memoria".to_owned(),
            peer_name: "Legacy Memoria".to_owned(),
            peer_base_url: "http://memoria.internal:8080".to_owned(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: String::new(),
            panel: None,
            granted_by: "operator".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
            reachability: conduit_link::Reachability::Unknown,
            last_probed_at: None,
        })
        .await
        .expect("stores");

    let (status, list) =
        json_response(call(&state, operator_get("/v1/linked-services")).await).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let rows = list.as_array().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["panel"]["label"], "Memoria");
}

// ── 3c: Generic + no panel is filtered from list ─────────────────────────

#[tokio::test]
async fn generic_row_without_a_panel_is_filtered_from_the_list() {
    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Generic,
            peer_id: "unlabelled".to_owned(),
            peer_name: "Unlabelled".to_owned(),
            peer_base_url: "http://unknown.internal:8080".to_owned(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: String::new(),
            panel: None,
            granted_by: "operator".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
            reachability: conduit_link::Reachability::Unknown,
            last_probed_at: None,
        })
        .await
        .expect("stores");

    let (status, list) =
        json_response(call(&state, operator_get("/v1/linked-services")).await).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let rows = list.as_array().expect("list");
    assert!(
        rows.is_empty(),
        "Generic without a panel has nothing to render; listing it would seed a broken tab: {list}"
    );
}

// ── 4a & 4b: proxy GET and POST-with-body ────────────────────────────────

#[tokio::test]
async fn proxy_forwards_get_to_the_peer_under_linked_services_prefix() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let response =
        call(&state, operator_get("/linked-services/household-memory/echo?tag=hello")).await;
    assert_eq!(response.status(), StatusCode::OK);

    let observed = peer.state.seen.lock().await.clone().expect("request observed");
    assert_eq!(observed.method, "GET");
    assert_eq!(observed.path_and_query, "/echo?tag=hello");
}

#[tokio::test]
async fn proxy_forwards_post_body_to_the_peer_verbatim() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let request = Request::builder()
        .method("POST")
        .uri("/linked-services/household-memory/echo")
        .header("authorization", OPERATOR_TOKEN)
        .header("content-type", "application/octet-stream")
        .body(Body::from(&b"payload-bytes"[..]))
        .expect("request");
    let response = call(&state, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(&bytes[..], b"payload-bytes");

    let observed = peer.state.seen.lock().await.clone().expect("request observed");
    assert_eq!(observed.method, "POST");
    assert_eq!(&observed.body[..], b"payload-bytes");
}

// ── 4c: streamed response body ───────────────────────────────────────────

#[tokio::test]
async fn proxy_forwards_a_streamed_response_body_end_to_end() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let response = call(&state, operator_get("/linked-services/household-memory/stream")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let collected =
        tokio::time::timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .expect("body arrives")
            .expect("body")
            .to_bytes();
    // Assembled from three chunks emitted by the peer's `/stream` handler.
    assert_eq!(&collected[..], b"chunk-1|chunk-2|chunk-3");
}

// ── 5a: Location rewrite — absolute-path ─────────────────────────────────

#[tokio::test]
async fn location_absolute_path_is_rewritten_under_linked_services_prefix() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let response =
        call(&state, operator_get("/linked-services/household-memory/redirect/absolute-path"))
            .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers().get("location"),
        Some(&HeaderValue::from_static("/linked-services/household-memory/ui?from=redirect"))
    );
}

// ── 5b: Location rewrite — same-origin absolute ──────────────────────────

#[tokio::test]
async fn location_same_origin_absolute_url_is_rewritten_to_a_local_prefix() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let response =
        call(&state, operator_get("/linked-services/household-memory/redirect/same-origin"))
            .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    let location =
        response.headers().get("location").expect("location").to_str().expect("utf8");
    // Same-origin absolute → local prefix; the peer's host/port MUST NOT
    // leak to the operator's browser.
    assert_eq!(location, "/linked-services/household-memory/ui/authorised");
    assert!(
        !location.contains(peer.base_url.trim_start_matches("http://")),
        "same-origin rewrite must strip the peer host: {location}"
    );
}

// ── 5c: Location rewrite — cross-origin passthrough ──────────────────────

#[tokio::test]
async fn location_cross_origin_absolute_url_is_passed_through_unchanged() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let response =
        call(&state, operator_get("/linked-services/household-memory/redirect/cross-origin"))
            .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers().get("location"),
        Some(&HeaderValue::from_static("https://cross.example.test/foreign"))
    );
}

// ── 6a: request header stripping ─────────────────────────────────────────

#[tokio::test]
async fn proxy_strips_hop_by_hop_and_operator_headers_on_the_forwarded_request() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let request = Request::builder()
        .method("POST")
        .uri("/linked-services/household-memory/echo")
        .header("authorization", OPERATOR_TOKEN)
        .header("host", "conduit.example.test")
        .header("connection", "keep-alive")
        .header("content-type", "application/octet-stream")
        .header("x-operator-hint", "carry-me")
        .body(Body::from(&b"secret-body"[..]))
        .expect("request");
    let response = call(&state, request).await;
    assert_eq!(response.status(), StatusCode::OK);

    let observed = peer.state.seen.lock().await.clone().expect("request observed");
    let names: Vec<&str> = observed.headers.iter().map(|(name, _)| name.as_str()).collect();
    // Operator's bearer, incoming Host, hop-by-hop Connection MUST NOT
    // reach the peer.
    assert!(
        !names.iter().any(|name| name.eq_ignore_ascii_case("authorization")),
        "authorization must be stripped, observed: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name.eq_ignore_ascii_case("connection")),
        "connection must be stripped, observed: {names:?}"
    );
    // Host is rewritten to the peer's host by the outbound HTTP client, not
    // forwarded from the operator's request. Assert the operator-supplied
    // value is gone (any Host present belongs to the peer address).
    let host_value = observed
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.as_str());
    if let Some(value) = host_value {
        assert_ne!(value, "conduit.example.test", "operator's Host must never reach the peer");
    }
    // Non-hop, non-operator headers ride through.
    assert!(
        names.iter().any(|name| name.eq_ignore_ascii_case("x-operator-hint")),
        "arbitrary custom headers must pass through, observed: {names:?}"
    );
}

// ── 6b: response header stripping ────────────────────────────────────────

#[tokio::test]
async fn proxy_strips_hop_by_hop_headers_from_the_upstream_response() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let request = Request::builder()
        .method("POST")
        .uri("/linked-services/household-memory/echo")
        .header("authorization", OPERATOR_TOKEN)
        .header("content-type", "application/octet-stream")
        .body(Body::from(&b"probe"[..]))
        .expect("request");
    let response = call(&state, request).await;
    assert_eq!(response.status(), StatusCode::OK);

    let headers = response.headers();
    let name_present = |needle: &str| {
        headers.iter().any(|(name, _)| name.as_str().eq_ignore_ascii_case(needle))
    };
    assert!(
        !name_present("connection"),
        "hop-by-hop connection header must be stripped from the response: {headers:?}"
    );
    assert!(
        !name_present("content-length"),
        "content-length must be stripped so a streaming body isn't truncated: {headers:?}"
    );
    // A benign upstream header MUST reach the operator's client.
    assert!(
        name_present("x-peer-extra"),
        "non-hop-by-hop upstream headers must pass through, observed: {headers:?}"
    );
}

// ── 7: operator-authenticated DELETE ────────────────────────────────────

#[tokio::test]
async fn operator_delete_removes_the_row() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let response = call(&state, operator_delete("/v1/linked-services/household-memory")).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(state.linked_service("household-memory").await.expect("store").is_none());
}

// ── 8a & 8b: peer-authenticated revoke ──────────────────────────────────

fn revoke_request(peer_id: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/v1/linked-services/{peer_id}/revoke"))
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .expect("request")
}

#[tokio::test]
async fn revoke_with_the_correct_sync_token_removes_the_row() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    let sync_token = seed_link(&state, "household-memory", &peer.base_url).await;

    let response = call(&state, revoke_request("household-memory", &sync_token)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(state.linked_service("household-memory").await.expect("store").is_none());
}

#[tokio::test]
async fn revoke_with_a_wrong_sync_token_is_unauthorized_and_row_is_preserved() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    seed_link(&state, "household-memory", &peer.base_url).await;

    let response = call(&state, revoke_request("household-memory", "not-the-real-token")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        state.linked_service("household-memory").await.expect("store").is_some(),
        "a failed revoke must leave the row intact"
    );
}

// ── reachability probe (issue #156) ─────────────────────────────────────

#[tokio::test]
async fn create_probes_a_live_health_endpoint_and_reports_reachable() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());

    let (status, _) = json_response(
        call(
            &state,
            operator_post_json("/v1/linked-services", memoria_link_body(&peer.base_url)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (list_status, list) =
        json_response(call(&state, operator_get("/v1/linked-services")).await).await;
    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list[0]["reachability"], "ok");
    assert!(list[0]["last_probed_at"].as_str().is_some(), "last_probed_at is populated");
}

#[tokio::test]
async fn create_against_a_dead_endpoint_still_returns_201_and_reports_unreachable() {
    // 127.0.0.1:1 is the reserved-well-known TCP echo port that no one binds
    // to in a test process — connect fails immediately without waiting for
    // the timeout. Spec 0005: failing probe MUST NOT block or remove the row.
    let state = AppState::new(EventBus::default());
    let mut body = memoria_link_body("http://127.0.0.1:1");
    body["peer_base_url"] = serde_json::json!("http://127.0.0.1:1");

    let started = std::time::Instant::now();
    let (status, _) =
        json_response(call(&state, operator_post_json("/v1/linked-services", body)).await)
            .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::CREATED);
    // Probe timeout is 3s; a connection refused resolves faster than that,
    // but we assert the whole flow doesn't stretch past twice the bound so
    // the create path stays snappy.
    assert!(
        elapsed < std::time::Duration::from_secs(6),
        "create must not stretch beyond twice the probe bound, took {elapsed:?}"
    );

    let (_, list) =
        json_response(call(&state, operator_get("/v1/linked-services")).await).await;
    assert_eq!(list[0]["reachability"], "unreachable");
    assert!(
        state.linked_service("household-memory").await.expect("store").is_some(),
        "a failed probe must never drop the row"
    );
}

#[tokio::test]
async fn startup_probe_flips_a_previously_unreachable_row_to_reachable() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Memoria,
            peer_id: "household-memory".to_owned(),
            peer_name: "Household Memory".to_owned(),
            peer_base_url: peer.base_url.clone(),
            sync_token_hash: hash_token("sync-token-real"),
            provider_definition_id: String::new(),
            panel: Some(LinkedServicePanel {
                id: "memoria".to_owned(),
                label: "Memoria".to_owned(),
                icon: "brain".to_owned(),
                path: "/ui/".to_owned(),
            }),
            granted_by: "operator".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
            reachability: conduit_link::Reachability::Unreachable,
            last_probed_at: None,
        })
        .await
        .expect("stores");

    conduit_api::linked_services::probe_all(&state).await;

    let (_, list) =
        json_response(call(&state, operator_get("/v1/linked-services")).await).await;
    assert_eq!(list[0]["reachability"], "ok");
}

// ── 9: replay resistance — the stored hash cannot masquerade as bearer ──

#[tokio::test]
async fn a_leaked_sync_token_hash_cannot_be_replayed_as_a_bearer_token() {
    let peer = spawn_peer().await;
    let state = AppState::new(EventBus::default());
    let sync_token = seed_link(&state, "household-memory", &peer.base_url).await;
    let leaked_hash = hash_token(&sync_token);

    let response = call(&state, revoke_request("household-memory", &leaked_hash)).await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "presenting the stored hash as a bearer MUST NOT authenticate; \
         otherwise a DB leak becomes a live credential"
    );
    assert!(
        state.linked_service("household-memory").await.expect("store").is_some(),
        "row must survive the replay attempt"
    );
}
