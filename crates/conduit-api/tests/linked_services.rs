//! `/v1/linked-services` — linking service panels into the console.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::linked_services::hash_token;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_link::LinkedServiceKind;
use conduit_provider::storage::LinkedService;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, json)
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", "Bearer test-management-token-abcdefghijklmnopqrstuvwxyz")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", "Bearer test-management-token-abcdefghijklmnopqrstuvwxyz")
        .body(Body::empty())
        .expect("request")
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("authorization", "Bearer test-management-token-abcdefghijklmnopqrstuvwxyz")
        .body(Body::empty())
        .expect("request")
}

fn link_body() -> serde_json::Value {
    serde_json::json!({
        "service_kind": "memoria",
        "peer_name": "Household Memory",
        "peer_id": "household-memory",
        "peer_base_url": "http://memoria.internal:8080",
        "panel": {
            "id": "memoria",
            "label": "Memoria",
            "icon": "brain",
            "path": "/ui/",
        }
    })
}

#[tokio::test]
async fn creating_a_linked_service_mints_a_sync_token_and_lists_the_panel() {
    let state = AppState::new(EventBus::default());

    let (status, body) = call(&state, post("/v1/linked-services", link_body())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let sync_token = body["sync_token"].as_str().expect("sync token").to_owned();

    let stored =
        state.linked_service("household-memory").await.expect("store").expect("stored row");
    assert_eq!(stored.service_kind.as_str(), "memoria");
    assert_eq!(stored.panel.as_ref().expect("panel").label, "Memoria");
    assert_eq!(stored.sync_token_hash, hash_token(&sync_token));

    let (status, body) = call(&state, get("/v1/linked-services")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body.as_array().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["peer_id"], "household-memory");
    assert_eq!(list[0]["panel"]["path"], "/ui/");
}

#[tokio::test]
async fn revoking_a_linked_service_by_sync_token_removes_it() {
    let state = AppState::new(EventBus::default());
    let (_, body) = call(&state, post("/v1/linked-services", link_body())).await;
    let sync_token = body["sync_token"].as_str().expect("sync token");
    let request = Request::builder()
        .method("POST")
        .uri("/v1/linked-services/household-memory/revoke")
        .header("authorization", format!("Bearer {sync_token}"))
        .body(Body::empty())
        .expect("request");

    let (status, _) = call(&state, request).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(state.linked_service("household-memory").await.expect("store").is_none());
}

#[tokio::test]
async fn deleting_a_linked_service_from_management_removes_it() {
    let state = AppState::new(EventBus::default());
    call(&state, post("/v1/linked-services", link_body())).await;

    let (status, _) = call(&state, delete("/v1/linked-services/household-memory")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(state.linked_service("household-memory").await.expect("store").is_none());
}

#[tokio::test]
async fn listing_legacy_vox_links_synthesizes_their_panel_manifest() {
    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Vox,
            peer_id: "kitchen-vox-01".to_owned(),
            peer_name: "Kitchen Vox".to_owned(),
            peer_base_url: "http://vox.internal:8081".to_owned(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: "vox-kitchen-vox-01".to_owned(),
            panel: None,
            granted_by: "Operator Console".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
        })
        .await
        .expect("stores");

    let (status, body) = call(&state, get("/v1/linked-services")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body.as_array().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["panel"]["label"], "Vox");
    assert_eq!(list[0]["panel"]["path"], "/ui/");
}

#[tokio::test]
async fn listing_legacy_instrumenta_links_synthesizes_their_panel_manifest() {
    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Instrumenta,
            peer_id: "instrumenta-main".to_owned(),
            peer_name: "Instrumenta".to_owned(),
            peer_base_url: "http://instrumenta.internal:8080".to_owned(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: String::new(),
            panel: None,
            granted_by: "Operator Console".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
        })
        .await
        .expect("stores");

    let (status, body) = call(&state, get("/v1/linked-services")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body.as_array().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["panel"]["label"], "Instrumenta");
    assert_eq!(list[0]["panel"]["icon"], "code");
}

#[tokio::test]
async fn listing_legacy_excita_links_synthesizes_their_panel_manifest() {
    let state = AppState::new(EventBus::default());
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Excita,
            peer_id: "excita-main".to_owned(),
            peer_name: "Excita".to_owned(),
            peer_base_url: "http://excita.internal:8080".to_owned(),
            sync_token_hash: "hash".to_owned(),
            provider_definition_id: String::new(),
            panel: None,
            granted_by: "Operator Console".to_owned(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
        })
        .await
        .expect("stores");

    let (status, body) = call(&state, get("/v1/linked-services")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = body.as_array().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["panel"]["label"], "Excita");
    assert_eq!(list[0]["panel"]["icon"], "radio");
}

fn vox_link_body() -> serde_json::Value {
    serde_json::json!({
        "service_kind": "vox",
        "peer_name": "Kitchen Vox",
        "peer_id": "kitchen-vox",
        "peer_base_url": "http://vox.internal:8081",
        "panel": {"id": "vox", "label": "Vox", "icon": "users", "path": "/ui/"},
        "extension": {"local_api_key": "vox-key-abc"}
    })
}

#[tokio::test]
async fn linking_a_vox_peer_auto_provisions_a_speaker_provider() {
    let state = AppState::new(EventBus::default());

    let (status, body) = call(&state, post("/v1/linked-services", vox_link_body())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let provider_id = body["extension"]["provider_definition_id"]
        .as_str()
        .expect("provider_definition_id in extension");
    assert_eq!(provider_id, "vox-kitchen-vox");

    let definition = state
        .provider_definition(provider_id)
        .await
        .expect("store")
        .expect("provider definition was auto-provisioned");
    assert_eq!(definition.label, "Conduit Vox — Kitchen Vox");
}

#[tokio::test]
async fn linking_a_vox_peer_without_extension_is_unprocessable() {
    let state = AppState::new(EventBus::default());

    let mut body = vox_link_body();
    body.as_object_mut().expect("object").remove("extension");
    let (status, _) = call(&state, post("/v1/linked-services", body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn linking_a_vox_peer_over_an_existing_provider_is_refused() {
    let state = AppState::new(EventBus::default());
    // First link succeeds and writes vox-kitchen-vox.
    call(&state, post("/v1/linked-services", vox_link_body())).await;
    // A retry without unlinking hits the row's own uniqueness first (409),
    // which is the same guard the pre-migration /v1/vox/links path had.
    let (status, _) = call(&state, post("/v1/linked-services", vox_link_body())).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn linking_a_service_publishes_a_linked_service_linked_event() {
    use conduit_core::event::Event;

    let state = AppState::new(EventBus::default());
    let mut subscription = state.bus.subscribe();

    let (status, _) = call(&state, post("/v1/linked-services", link_body())).await;
    assert_eq!(status, StatusCode::CREATED);

    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
        .await
        .expect("event within a second")
        .expect("event received");

    match &envelope.event {
        Event::LinkedServiceLinked { peer_id, peer_name, service_kind } => {
            assert_eq!(peer_id, "household-memory");
            assert_eq!(peer_name, "Household Memory");
            assert_eq!(service_kind, "memoria");
        }
        other => panic!("expected LinkedServiceLinked, got {other:?}"),
    }
}

#[tokio::test]
async fn deleting_a_linked_service_publishes_a_linked_service_unlinked_event() {
    use conduit_core::event::Event;

    let state = AppState::new(EventBus::default());
    call(&state, post("/v1/linked-services", link_body())).await;

    // Subscribe after the create so we only see the delete's event.
    let mut subscription = state.bus.subscribe();
    let (status, _) = call(&state, delete("/v1/linked-services/household-memory")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
        .await
        .expect("event within a second")
        .expect("event received");

    match &envelope.event {
        Event::LinkedServiceUnlinked { peer_id } => {
            assert_eq!(peer_id, "household-memory");
        }
        other => panic!("expected LinkedServiceUnlinked, got {other:?}"),
    }
}
