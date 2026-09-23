//! Conditional roster requests over a real HTTP connection.

use axum::http::header::{ETAG, IF_NONE_MATCH};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_provider::storage::EnrolledSpeaker;
use tokio::net::TcpListener;

#[tokio::test]
async fn roster_etag_round_trip_stays_stable_until_a_speaker_changes() {
    let state = AppState::new(EventBus::default());
    state.put_speaker(EnrolledSpeaker::named("Ada")).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(server_state)).await.unwrap();
    });

    let client = reqwest::Client::new();
    let first = client.get(format!("{base_url}/v1/speakers")).send().await.unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let first_etag = first.headers().get(ETAG).unwrap().clone();
    let first_body: serde_json::Value = first.json().await.unwrap();
    assert_eq!(first_body.as_array().unwrap().len(), 1);

    let unchanged = client
        .get(format!("{base_url}/v1/speakers"))
        .header(IF_NONE_MATCH, &first_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(unchanged.status(), reqwest::StatusCode::NOT_MODIFIED);
    assert!(unchanged.bytes().await.unwrap().is_empty());

    state.put_speaker(EnrolledSpeaker::named("Grace")).await.unwrap();
    let changed = client
        .get(format!("{base_url}/v1/speakers"))
        .header(IF_NONE_MATCH, &first_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), reqwest::StatusCode::OK);
    assert_ne!(changed.headers().get(ETAG).unwrap(), &first_etag);
    let changed_body: serde_json::Value = changed.json().await.unwrap();
    assert_eq!(changed_body.as_array().unwrap().len(), 2);

    server.abort();
}
