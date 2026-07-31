//! Checks that the SSE endpoint really carries bus events to a client.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::event::{Envelope, Event};
use conduit_core::id::TraceId;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Opens the stream and returns its body. The handler subscribes before
/// responding, so events published after this call are not missed.
async fn open(uri: &str, state: &AppState) -> Body {
    let request = Request::builder().uri(uri).body(Body::empty()).expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body()
}

/// Reads frames until one carries data, or gives up.
async fn next_message(body: &mut Body) -> String {
    let read = async {
        loop {
            let frame = body.frame().await.expect("stream open").expect("frame");
            if let Ok(data) = frame.into_data() {
                return String::from_utf8(data.to_vec()).expect("utf-8");
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read).await.expect("message arrives")
}

#[tokio::test]
async fn stream_carries_published_events() {
    let state = AppState::new(EventBus::default());
    let mut body = open("/v1/events", &state).await;

    state.bus.publish(Envelope::new(TraceId::new(), Event::LlmToken { delta: "hi".into() }));

    let message = next_message(&mut body).await;
    assert!(message.contains("event: LlmToken"), "unexpected message: {message}");
    assert!(message.contains(r#""delta":"hi""#), "unexpected message: {message}");
}

#[tokio::test]
async fn stream_honours_stage_filters() {
    let state = AppState::new(EventBus::default());
    let mut body = open("/v1/events?stages=reasoning", &state).await;

    // Filtered out; must not appear before the reasoning event below.
    state.bus.publish(Envelope::new(TraceId::new(), Event::ConversationStarted));
    state.bus.publish(Envelope::new(TraceId::new(), Event::LlmToken { delta: "hi".into() }));

    let message = next_message(&mut body).await;
    assert!(message.contains("event: LlmToken"), "unexpected message: {message}");
    assert!(!message.contains("ConversationStarted"), "filter leaked: {message}");
}

#[tokio::test]
async fn stream_honours_trace_filters() {
    let state = AppState::new(EventBus::default());
    let mine = TraceId::new();
    let mut body = open(&format!("/v1/events?trace={mine}"), &state).await;

    state.bus.publish(Envelope::new(TraceId::new(), Event::ConversationStarted));
    state.bus.publish(Envelope::new(mine, Event::ConversationCompleted));

    let message = next_message(&mut body).await;
    assert!(message.contains("event: ConversationCompleted"), "unexpected: {message}");
    assert!(message.contains(&mine.to_string()), "wrong trace: {message}");
}
