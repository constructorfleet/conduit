//! `/vox/*` reverse proxy for the linked Conduit Vox peer.
//!
//! The branch plan committed to three things worth pinning down in tests:
//! requests only work when there is one linked peer, Conduit adds the stored
//! Vox API key rather than forwarding the operator credential, and redirects
//! come back pointing at Conduit rather than at the hidden peer.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_link::{LinkedServiceKind, LinkedServicePanel};
use conduit_provider::storage::{
    LinkedService, ProviderDefinition, ProviderDefinitionVariant, ProviderSecret,
    SpeakerEngine, SpeakerIdVariant,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn call(state: &AppState, request: Request<Body>) -> axum::response::Response {
    router(state.clone()).oneshot(request).await.expect("router responds")
}

fn get_request(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
}

fn post_request(uri: &str, body: &'static [u8]) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/octet-stream")
        .body(Body::from(body))
        .expect("request")
}

#[tokio::test]
async fn proxy_returns_not_found_when_no_vox_peer_is_linked() {
    let state = AppState::new(EventBus::default());

    let response = call(&state, get_request("/vox/ui")).await;
    let status = response.status();
    let body = response.into_body().collect().await.expect("body").to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        json["detail"].as_str().is_some_and(|detail| detail.contains("link")),
        "the body should point at the link flow: {json}"
    );
}

#[tokio::test]
async fn proxy_forwards_the_request_with_the_stored_vox_api_key() {
    let seen = Arc::new(tokio::sync::Mutex::new(None::<ObservedRequest>));
    let upstream = vox_peer(seen.clone()).await;

    let state = AppState::new(EventBus::default());
    install_link_provider(&state, &upstream.base_url).await;
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Vox,
            peer_id: "kitchen".to_owned(),
            peer_name: "Kitchen Vox".to_owned(),
            peer_base_url: upstream.base_url.clone(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: "vox-kitchen".to_owned(),
            panel: Some(LinkedServicePanel {
                id: "vox".to_owned(),
                label: "Vox".to_owned(),
                icon: "users".to_owned(),
                path: "/ui/".to_owned(),
            }),
            granted_by: "operator".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
        })
        .await
        .expect("stores");

    let mut request = post_request("/vox/identify?confidence=1", b"test-audio");
    request
        .headers_mut()
        .insert("authorization", HeaderValue::from_static("Bearer operator-secret"));
    let response = call(&state, request).await;
    let status = response.status();
    let body = response.into_body().collect().await.expect("body").to_bytes();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"proxied");

    let seen = seen.lock().await.clone().expect("request observed");
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.path_and_query, "/identify?confidence=1");
    assert_eq!(seen.authorization.as_deref(), Some("Bearer stored-vox-key"));
    assert_eq!(seen.body, b"test-audio");
}

#[tokio::test]
async fn proxy_rewrites_redirect_locations_back_under_conduit() {
    let upstream = vox_peer(Arc::new(tokio::sync::Mutex::new(None))).await;

    let state = AppState::new(EventBus::default());
    install_link_provider(&state, &upstream.base_url).await;
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Vox,
            peer_id: "kitchen".to_owned(),
            peer_name: "Kitchen Vox".to_owned(),
            peer_base_url: upstream.base_url.clone(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: "vox-kitchen".to_owned(),
            panel: Some(LinkedServicePanel {
                id: "vox".to_owned(),
                label: "Vox".to_owned(),
                icon: "users".to_owned(),
                path: "/ui/".to_owned(),
            }),
            granted_by: "operator".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
        })
        .await
        .expect("stores");

    let response = call(&state, get_request("/vox/redirect")).await;

    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers().get("location"),
        Some(&HeaderValue::from_static("/vox/ui?from=redirect"))
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservedRequest {
    method: String,
    path_and_query: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

struct VoxPeer {
    base_url: String,
}

async fn install_link_provider(state: &AppState, base_url: &str) {
    state
        .put_provider_definition(
            "vox-kitchen",
            ProviderDefinition {
                id: "vox-kitchen".to_owned(),
                label: "Kitchen Vox".to_owned(),
                variant: ProviderDefinitionVariant::SpeakerId {
                    variant: SpeakerIdVariant::Http {
                        base_url: base_url.to_owned(),
                        api_key: Some(ProviderSecret::Inline {
                            value: "stored-vox-key".to_owned(),
                        }),
                        engine: SpeakerEngine::SpeechBrain,
                        threshold_percent: 50,
                    },
                },
                settings: serde_json::Map::new(),
            },
        )
        .await
        .expect("stores provider definition");
}

async fn vox_peer(seen: Arc<tokio::sync::Mutex<Option<ObservedRequest>>>) -> VoxPeer {
    async fn identify(
        State(seen): State<Arc<tokio::sync::Mutex<Option<ObservedRequest>>>>,
        request: Request<Body>,
    ) -> impl IntoResponse {
        let (parts, body) = request.into_parts();
        let bytes = body.collect().await.expect("body").to_bytes();
        *seen.lock().await = Some(ObservedRequest {
            method: parts.method.to_string(),
            path_and_query: parts
                .uri
                .path_and_query()
                .map_or_else(|| parts.uri.path().to_owned(), |value| value.as_str().to_owned()),
            authorization: parts
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            body: bytes.to_vec(),
        });
        (StatusCode::OK, "proxied")
    }

    async fn redirect() -> impl IntoResponse {
        (StatusCode::TEMPORARY_REDIRECT, [("location", "/ui?from=redirect")])
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let address = listener.local_addr().expect("listener address");
    let app = Router::new()
        .route("/identify", post(identify))
        .route("/redirect", get(redirect))
        .with_state(seen);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("peer serves");
    });

    VoxPeer { base_url: format!("http://{address}") }
}

#[tokio::test]
async fn linked_services_proxy_injects_the_row_proxy_auth_bearer() {
    // Spec 0005 strips the operator's authorization header before the proxy
    // forwards a request, so an iframed peer UI otherwise couldn't
    // authenticate to its own service. The row's proxy_auth_bearer fills
    // that gap: Conduit swaps the operator bearer for the peer-scoped one.
    let seen = Arc::new(tokio::sync::Mutex::new(None));
    let upstream = vox_peer(seen.clone()).await;

    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Vox,
            peer_id: "kitchen".to_owned(),
            peer_name: "Kitchen Vox".to_owned(),
            peer_base_url: upstream.base_url.clone(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: String::new(),
            panel: Some(LinkedServicePanel {
                id: "vox".to_owned(),
                label: "Vox".to_owned(),
                icon: "users".to_owned(),
                path: "/ui/".to_owned(),
            }),
            granted_by: "operator".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: Some("peer-scoped-key".to_owned()),
        })
        .await
        .expect("stores");

    let mut request = post_request("/linked-services/kitchen/identify", b"test");
    request
        .headers_mut()
        .insert("authorization", HeaderValue::from_static("Bearer operator-secret"));
    let response = call(&state, request).await;

    assert_eq!(response.status(), StatusCode::OK);
    let observed = seen.lock().await.clone().expect("request observed");
    assert_eq!(
        observed.authorization.as_deref(),
        Some("Bearer peer-scoped-key"),
        "proxy must swap the operator bearer for the peer's stored key"
    );
}

#[tokio::test]
async fn linked_services_proxy_forwards_no_authorization_when_none_stored() {
    // A peer that doesn't need bearer auth (e.g. Memoria running open on the
    // internal network) has proxy_auth_bearer=None. The proxy still strips
    // the operator bearer per 0005, but doesn't invent a replacement.
    let seen = Arc::new(tokio::sync::Mutex::new(None));
    let upstream = vox_peer(seen.clone()).await;

    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Memoria,
            peer_id: "memoria".to_owned(),
            peer_name: "Memoria".to_owned(),
            peer_base_url: upstream.base_url.clone(),
            sync_token_hash: "hash".to_owned(),
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
        })
        .await
        .expect("stores");

    let mut request = post_request("/linked-services/memoria/identify", b"test");
    request
        .headers_mut()
        .insert("authorization", HeaderValue::from_static("Bearer operator-secret"));
    let response = call(&state, request).await;

    assert_eq!(response.status(), StatusCode::OK);
    let observed = seen.lock().await.clone().expect("request observed");
    assert_eq!(
        observed.authorization, None,
        "operator bearer stripped, no replacement injected"
    );
}
