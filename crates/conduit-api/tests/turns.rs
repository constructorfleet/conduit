//! End-to-end checks for the server-owned turn reconstruction API.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::event::{Envelope, Event};
use conduit_core::id::{ConversationId, ToolCallId, TraceId, TurnId};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn call(state: &AppState, uri: &str) -> (StatusCode, serde_json::Value) {
    let request = Request::builder().uri(uri).body(Body::empty()).expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json response")
    };
    (status, body)
}

fn publish(state: &AppState, trace: TraceId, conversation: ConversationId, event: Event) {
    state.bus.publish(
        Envelope::new(trace, event).with_conversation(conversation).with_pipeline("kitchen"),
    );
}

async fn wait_for(
    state: &AppState,
    uri: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let started = tokio::time::Instant::now();
    loop {
        let (status, body) = call(state, uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if predicate(&body) {
            return body;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "turn reconstruction never reached expected state: {body}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn lists_and_fetches_reconstructed_turns_without_sensitive_tool_payloads() {
    let state = AppState::new(EventBus::default());
    let trace = TraceId::new();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    let tool_call = ToolCallId::new("call_weather");

    publish(&state, trace, conversation, Event::ConversationStarted);
    publish(&state, trace, conversation, Event::TurnStarted { turn });
    publish(&state, trace, conversation, Event::LlmToken { delta: "I'll check. ".into() });
    publish(
        &state,
        trace,
        conversation,
        Event::UtteranceSegmentStarted {
            segment: "assistant-preamble-1".into(),
            role: conduit_core::event::UtteranceSegmentRole::AssistantPreamble,
            modality: conduit_core::graph::Modality::Audio,
            text: "I'll check.".into(),
        },
    );
    publish(
        &state,
        trace,
        conversation,
        Event::ToolBatchStarted {
            batch: "round-1".into(),
            calls: vec![tool_call.clone()],
            model_round: 1,
        },
    );
    publish(
        &state,
        trace,
        conversation,
        Event::ToolRequested { call: tool_call.clone(), name: "weather.get".into() },
    );
    publish(&state, trace, conversation, Event::ToolStarted { call: tool_call.clone() });
    publish(
        &state,
        trace,
        conversation,
        Event::ToolCompleted { call: tool_call, duration_ms: 12 },
    );
    publish(&state, trace, conversation, Event::ConversationCompleted);

    let body = wait_for(&state, "/v1/turns", |body| {
        body["turns"].as_array().is_some_and(|turns| turns.len() == 1)
    })
    .await;
    assert_eq!(body["turns"][0]["turn_id"], turn.to_string());
    assert_eq!(body["turns"][0]["conversation_id"], conversation.to_string());
    assert_eq!(body["turns"][0]["pipeline_name"], "kitchen");
    assert_eq!(body["turns"][0]["status"], "completed");

    let (_, snapshot) = call(&state, &format!("/v1/turns/{turn}")).await;
    assert_eq!(snapshot["turn_id"], turn.to_string());
    assert_eq!(snapshot["items"][0]["kind"], "utterance_segment");
    assert_eq!(snapshot["items"][0]["role"], "assistant_preamble");
    assert_eq!(snapshot["items"][1]["kind"], "tool_batch");
    assert_eq!(snapshot["items"][1]["calls"][0]["name"], "weather.get");
    assert_eq!(snapshot["items"][1]["calls"][0]["status"], "completed");
    assert!(
        snapshot.to_string().find("arguments").is_none(),
        "default reconstruction exposed sensitive tool payloads: {snapshot}"
    );
}

#[tokio::test]
async fn raw_events_for_a_turn_remain_a_separate_evidence_route() {
    let state = AppState::new(EventBus::default());
    let trace = TraceId::new();
    let conversation = ConversationId::new();
    let turn = TurnId::new();

    publish(&state, trace, conversation, Event::TurnStarted { turn });
    publish(
        &state,
        trace,
        conversation,
        Event::StageFailed {
            node: "tts".into(),
            error: "connection refused".into(),
            recovered: false,
        },
    );

    wait_for(&state, "/v1/turns", |body| {
        body["turns"].as_array().is_some_and(|turns| turns.len() == 1)
    })
    .await;
    let body = wait_for(&state, &format!("/v1/turns/{turn}/events"), |body| {
        body["events"].as_array().is_some_and(|events| events.len() == 2)
    })
    .await;
    assert_eq!(body["turn_id"], turn.to_string());
    assert_eq!(body["events"][1]["event"]["type"], "StageFailed");
    assert_eq!(body["events"][1]["event"]["error"], "connection refused");
}

#[tokio::test]
async fn completed_turn_history_respects_count_retention() {
    let state = AppState::new(EventBus::default()).with_turn_history_retention(
        conduit_api::turns::TurnHistoryRetention { max_turns: Some(1), max_age: None },
    );
    let first = TurnId::new();
    let second = TurnId::new();

    for turn in [first, second] {
        let trace = TraceId::new();
        let conversation = ConversationId::new();
        publish(&state, trace, conversation, Event::TurnStarted { turn });
        publish(&state, trace, conversation, Event::ConversationCompleted);
    }

    let body = wait_for(&state, "/v1/turns", |body| {
        body["turns"].as_array().is_some_and(|turns| turns.len() == 1)
    })
    .await;
    assert_eq!(body["turns"][0]["turn_id"], second.to_string());

    let (status, _) = call(&state, &format!("/v1/turns/{first}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn live_stream_update_carries_route_identity_and_sequence() {
    let state = AppState::new(EventBus::default());
    let response = router(state.clone())
        .oneshot(Request::builder().uri("/v1/turns/live").body(Body::empty()).expect("request"))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();

    let trace = TraceId::new();
    let conversation = ConversationId::new();
    let turn = TurnId::new();
    publish(&state, trace, conversation, Event::TurnStarted { turn });

    let read = async {
        loop {
            let frame = body.frame().await.expect("stream open").expect("frame");
            if let Ok(data) = frame.into_data() {
                let text = String::from_utf8(data.to_vec()).expect("utf-8");
                if text.contains("event: turn_reconstruction") {
                    return text;
                }
            }
        }
    };
    let message = tokio::time::timeout(Duration::from_secs(5), read).await.expect("message");
    assert!(message.contains(&turn.to_string()), "{message}");
    assert!(message.contains(&conversation.to_string()), "{message}");
    assert!(message.contains(r#""pipeline_name":"kitchen""#), "{message}");
    assert!(message.contains(r#""sequence":1"#), "{message}");
}

#[test]
fn turn_history_retention_defaults_bound_count_and_age() {
    let retention = conduit_api::config::turn_history_retention_from_vars(&Default::default())
        .expect("default retention");
    assert_eq!(retention.max_turns, Some(500));
    assert_eq!(retention.max_age, Some(Duration::from_secs(86_400)));
}

#[test]
fn turn_history_retention_zero_removes_individual_bounds() {
    let vars = [
        ("CONDUIT_TURN_HISTORY_MAX_TURNS".to_owned(), "0".to_owned()),
        ("CONDUIT_TURN_HISTORY_RETENTION_SECS".to_owned(), "0".to_owned()),
    ]
    .into_iter()
    .collect();
    let retention =
        conduit_api::config::turn_history_retention_from_vars(&vars).expect("retention");
    assert_eq!(retention.max_turns, None);
    assert_eq!(retention.max_age, None);
}

#[test]
fn turn_history_retention_rejects_invalid_values() {
    let vars = [("CONDUIT_TURN_HISTORY_MAX_TURNS".to_owned(), "many".to_owned())]
        .into_iter()
        .collect();
    let error = conduit_api::config::turn_history_retention_from_vars(&vars)
        .expect_err("invalid count is refused");
    assert!(error.to_string().contains("CONDUIT_TURN_HISTORY_MAX_TURNS"), "{error}");

    let vars = [("CONDUIT_TURN_HISTORY_RETENTION_SECS".to_owned(), "1 day".to_owned())]
        .into_iter()
        .collect();
    let error = conduit_api::config::turn_history_retention_from_vars(&vars)
        .expect_err("invalid duration is refused");
    assert!(error.to_string().contains("CONDUIT_TURN_HISTORY_RETENTION_SECS"), "{error}");
}

#[test]
fn turn_summary_has_stable_contract_time_fields() {
    let at = Utc::now();
    assert!(at.to_rfc3339().contains('T'));
}
