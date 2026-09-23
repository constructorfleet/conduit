//! HTTP and event-bus contract tests for linked Excita wake events.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::event::Event;
use conduit_link::{LinkedServiceKind, Reachability};
use conduit_provider::storage::LinkedService;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn state() -> (AppState, conduit_core::bus::Subscription) {
    let bus = EventBus::default();
    let subscription = bus.subscribe();
    let state = AppState::new(bus);
    state
        .put_linked_service(LinkedService {
            service_kind: LinkedServiceKind::Excita,
            peer_id: "excita-kitchen".into(),
            peer_name: "Kitchen Excita".into(),
            peer_base_url: "http://excita:8080".into(),
            sync_token_hash: hash("excita-sync-token"),
            peer_token_hash: None,
            peer_token_ciphertext: None,
            capabilities: vec!["excita.wake-events".into()],
            capability_endpoints: Default::default(),
            provider_definition_id: String::new(),
            panel: None,
            granted_by: "operator".into(),
            granted_at: chrono::Utc::now(),
            last_seen: None,
            proxy_auth_bearer: None,
            reachability: Reachability::Unknown,
            last_probed_at: None,
        })
        .await
        .expect("store link");
    (state, subscription)
}

fn request(token: &str, peer_id: &str, key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/wake-events")
        .header("authorization", format!("Bearer {token}"))
        .header("idempotency-key", key)
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"event_type":"detected","peer_id":"{peer_id}","phrase":"hey conduit","confidence":0.91,"detected_at":"2026-08-10T14:22:03.412Z","source_device":"2a09b967-e66b-4a5f-8f2d-80baa8df9ec1","audio_clip_ref":"clip-1"}}"#
        )))
        .expect("request")
}

#[tokio::test]
async fn authenticated_wake_event_is_published_with_its_device_and_duplicate_is_ignored() {
    let (state, mut events) = state().await;
    let response = router(state.clone())
        .oneshot(request("excita-sync-token", "excita-kitchen", "fire-1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(response.into_body().collect().await.unwrap().to_bytes().is_empty());
    let envelope = events.recv().await.expect("published event");
    assert!(
        matches!(envelope.event, Event::WakeWordDetected { ref phrase, confidence, source_device: Some(ref source_device), audio_clip_ref: Some(ref clip), .. } if phrase == "hey conduit" && confidence == 0.91 && source_device == "2a09b967-e66b-4a5f-8f2d-80baa8df9ec1" && clip == "clip-1")
    );
    assert_eq!(envelope.device.unwrap().to_string(), "2a09b967-e66b-4a5f-8f2d-80baa8df9ec1");

    let response = router(state.clone())
        .oneshot(request("excita-sync-token", "excita-kitchen", "fire-1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let duplicate =
        tokio::time::timeout(std::time::Duration::from_millis(20), events.recv()).await;
    assert!(duplicate.is_err(), "duplicate emitted another event: {duplicate:?}");
}

#[tokio::test]
async fn wake_event_rejects_bad_credentials_and_mismatched_peer_identity() {
    let (state, _) = state().await;
    let response = router(state.clone())
        .oneshot(request("wrong", "excita-kitchen", "fire-1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router(state)
        .oneshot(request("excita-sync-token", "another-peer", "fire-2"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn wake_event_retry_is_deduplicated_over_real_http() {
    let (state, mut events) = state().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server =
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "event_type": "rejected",
        "peer_id": "excita-kitchen",
        "phrase": "hey conduit",
        "confidence": 0.2,
        "detected_at": "2026-08-10T14:22:03.412Z",
        "source_device": "2a09b967-e66b-4a5f-8f2d-80baa8df9ec1",
        "audio_clip_ref": "clip-2"
    });
    let send = || {
        client
            .post(format!("http://{address}/v1/wake-events"))
            .bearer_auth("excita-sync-token")
            .header("Idempotency-Key", "fire-http-1")
            .json(&body)
    };
    assert_eq!(send().send().await.unwrap().status(), StatusCode::ACCEPTED);
    assert_eq!(send().send().await.unwrap().status(), StatusCode::ACCEPTED);
    assert!(matches!(events.recv().await.unwrap().event, Event::WakeWordRejected { .. }));
    assert!(tokio::time::timeout(std::time::Duration::from_millis(20), events.recv())
        .await
        .is_err());
    server.abort();
}
