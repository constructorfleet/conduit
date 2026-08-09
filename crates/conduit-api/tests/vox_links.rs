//! `/v1/vox/links` — establishing, listing, and revoking a Conduit Vox link.
//!
//! The interesting cases are the ones the plan committed to: the sync token is
//! returned exactly once, the row that survives only carries its hash, and
//! auto-provisioning a provider that would overwrite a hand-authored one
//! refuses rather than silently trampling.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::vox::hash_token;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_provider::storage::{
    ProviderDefinition, ProviderDefinitionVariant, SpeakerIdVariant,
};
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

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder().method("DELETE").uri(uri).body(Body::empty()).expect("request")
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request")
}

fn link_body() -> serde_json::Value {
    serde_json::json!({
        "peer_name": "Kitchen Vox",
        "peer_id": "kitchen-vox-01",
        "vox_base_url": "http://vox.internal:8081",
        "vox_api_key": "vox-key-abcdefghijklmnop",
    })
}

#[tokio::test]
async fn creating_a_link_mints_a_token_and_auto_provisions_a_provider() {
    let state = AppState::new(EventBus::default());

    let (status, body) = call(&state, post("/v1/vox/links", link_body())).await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    let sync_token = body["sync_token"].as_str().expect("a sync token").to_owned();
    let provider_id =
        body["provider_definition_id"].as_str().expect("a provider id").to_owned();
    assert!(sync_token.len() >= 40, "the token should carry real entropy");
    assert!(provider_id.starts_with("vox-"), "the id should name the peer: {provider_id}");

    let stored = state
        .provider_definition(&provider_id)
        .await
        .expect("store")
        .expect("the definition exists");
    assert!(matches!(
        stored.variant,
        ProviderDefinitionVariant::SpeakerId { variant: SpeakerIdVariant::Http { .. } }
    ));

    let link = state.vox_link("kitchen-vox-01").await.expect("store").expect("the link exists");
    assert_eq!(link.provider_definition_id, provider_id);
    assert_eq!(
        link.sync_token_hash,
        hash_token(&sync_token),
        "the row must carry the hash of the returned token"
    );
    assert!(
        !link.sync_token_hash.contains(&sync_token),
        "the row must never carry the raw token"
    );
}

#[tokio::test]
async fn listing_links_never_reveals_the_sync_token() {
    let state = AppState::new(EventBus::default());
    call(&state, post("/v1/vox/links", link_body())).await;

    let (status, body) = call(&state, get("/v1/vox/links")).await;

    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().expect("a list");
    assert_eq!(list.len(), 1);
    let entry = &list[0];
    assert_eq!(entry["peer_id"], "kitchen-vox-01");
    assert_eq!(entry["peer_name"], "Kitchen Vox");
    assert_eq!(entry["peer_base_url"], "http://vox.internal:8081");
    assert!(entry.get("sync_token").is_none(), "the token was minted, not re-shown");
    assert!(entry.get("sync_token_hash").is_none(), "the hash is an implementation detail");
}

#[tokio::test]
async fn linking_the_same_peer_twice_is_refused_until_the_first_is_revoked() {
    // Otherwise a rushed operator would silently overwrite the sync token the
    // peer is already using, and Vox would start failing to sync without a
    // clear reason.
    let state = AppState::new(EventBus::default());
    let (status, _) = call(&state, post("/v1/vox/links", link_body())).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = call(&state, post("/v1/vox/links", link_body())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn a_link_that_would_overwrite_a_hand_authored_provider_refuses() {
    // The provisioned id is deterministic, so a peer whose name collides with
    // an existing definition must lose the race rather than replace one an
    // operator wrote by hand.
    let state = AppState::new(EventBus::default());
    let existing = ProviderDefinition {
        id: "vox-kitchen-vox-01".to_owned(),
        label: "hand-authored".to_owned(),
        variant: ProviderDefinitionVariant::SpeakerId {
            variant: SpeakerIdVariant::Http {
                base_url: "http://other".to_owned(),
                api_key: None,
                engine: conduit_provider::storage::SpeakerEngine::SpeechBrain,
                threshold_percent: 50,
            },
        },
        settings: serde_json::Map::new(),
    };
    let existing_id = existing.id.clone();
    state.put_provider_definition(&existing_id, existing).await.expect("stores");

    let (status, body) = call(&state, post("/v1/vox/links", link_body())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // The hand-authored definition survives, and the link was not stored.
    let survivor = state
        .provider_definition("vox-kitchen-vox-01")
        .await
        .expect("store")
        .expect("still there");
    assert_eq!(survivor.label, "hand-authored");
    assert!(state.vox_link("kitchen-vox-01").await.expect("store").is_none());
}

#[tokio::test]
async fn deleting_a_link_revokes_the_row_but_keeps_the_provider() {
    let state = AppState::new(EventBus::default());
    let (_, body) = call(&state, post("/v1/vox/links", link_body())).await;
    let provider_id = body["provider_definition_id"].as_str().expect("provider id").to_owned();

    let (status, _) = call(&state, delete("/v1/vox/links/kitchen-vox-01")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(state.vox_link("kitchen-vox-01").await.expect("store").is_none());
    assert!(
        state.provider_definition(&provider_id).await.expect("store").is_some(),
        "the provider survives so pipelines that named it keep working"
    );
}

#[tokio::test]
async fn deleting_a_link_that_does_not_exist_is_a_not_found() {
    let state = AppState::new(EventBus::default());
    let (status, _) = call(&state, delete("/v1/vox/links/never-linked")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_fields_are_reported_as_unprocessable() {
    let state = AppState::new(EventBus::default());
    let body = serde_json::json!({
        "peer_name": "  ",
        "peer_id": "kitchen",
        "vox_base_url": "http://vox.internal",
        "vox_api_key": "k",
    });
    let (status, _) = call(&state, post("/v1/vox/links", body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn peer_ids_that_would_traverse_the_filesystem_are_refused() {
    let state = AppState::new(EventBus::default());
    let body = serde_json::json!({
        "peer_name": "peer",
        "peer_id": "../etc/passwd",
        "vox_base_url": "http://vox.internal",
        "vox_api_key": "k",
    });
    let (status, _) = call(&state, post("/v1/vox/links", body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
