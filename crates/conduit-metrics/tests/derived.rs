//! What a scrape says after a pipeline has run.

use std::sync::Arc;
use std::time::Duration;

use conduit_core::bus::EventBus;
use conduit_core::event::{CancelReason, Envelope, Event, FinishReason};
use conduit_core::id::{ConversationId, ToolCallId, TraceId};
use conduit_metrics::{Collector, Metrics};

/// Publishes `events` for one conversation and returns the resulting scrape.
async fn scrape(events: Vec<Event>) -> String {
    let bus = EventBus::default();
    let metrics = Arc::new(Metrics::new());
    Collector::spawn(Arc::clone(&metrics), &bus);

    let conversation = ConversationId::new();
    let trace = TraceId::new();
    for event in events {
        bus.publish(Envelope::new(trace, event).with_conversation(conversation));
    }

    // The collector is a separate task; give it the events before scraping.
    drop(bus);
    tokio::time::sleep(Duration::from_millis(50)).await;
    metrics.render()
}

/// A complete turn.
fn a_turn() -> Vec<Event> {
    vec![
        Event::ConversationStarted,
        Event::SpeechFinal { text: "hello".into(), confidence: None, language: None },
        Event::LlmRequestStarted { model: "test".into() },
        Event::LlmToken { delta: "hi".into() },
        Event::LlmFinished {
            reason: FinishReason::Stop,
            prompt_tokens: Some(11),
            completion_tokens: Some(3),
        },
        Event::TtsStarted { voice: "default".into() },
        Event::AudioStreaming { sequence: 0, bytes: 640 },
        Event::AudioStreaming { sequence: 1, bytes: 640 },
        Event::TtsFinished { duration_ms: 40 },
        Event::ConversationCompleted,
    ]
}

#[tokio::test]
async fn events_are_counted_by_stage() {
    let body = scrape(a_turn()).await;
    assert!(body.contains("conduit_events_total{stage=\"reasoning\"} 3"), "{body}");
    assert!(body.contains("conduit_events_total{stage=\"synthesis\"} 4"), "{body}");
    assert!(body.contains("conduit_events_total{stage=\"transcription\"} 1"), "{body}");
}

#[tokio::test]
async fn a_completed_turn_is_counted_and_timed() {
    let body = scrape(a_turn()).await;
    assert!(body.contains("conduit_conversations_total{outcome=\"completed\"} 1"), "{body}");
    assert!(
        body.contains("conduit_turn_duration_seconds_count{outcome=\"completed\"} 1"),
        "{body}"
    );
}

#[tokio::test]
async fn time_to_first_audio_is_measured_once_per_turn() {
    // The latency that matters for a voice assistant is when it started
    // speaking, not when it finished — and only the first chunk answers that.
    let body = scrape(a_turn()).await;
    assert!(body.contains("conduit_time_to_first_audio_seconds_count 1"), "{body}");
}

#[tokio::test]
async fn a_turn_that_never_speaks_records_no_speech_latency() {
    let body = scrape(vec![
        Event::ConversationStarted,
        Event::ConversationCancelled { reason: CancelReason::Error },
    ])
    .await;
    // A histogram with no observations exposes no series at all, rather than
    // a zero — a zero would imply an answer arrived instantly.
    assert!(!body.contains("conduit_time_to_first_audio_seconds_count"), "{body}");
    assert!(!body.contains("conduit_time_to_first_audio_seconds_bucket"), "{body}");
}

#[tokio::test]
async fn cancellations_are_counted_by_reason() {
    let body = scrape(vec![
        Event::ConversationStarted,
        Event::ConversationCancelled { reason: CancelReason::BargeIn },
    ])
    .await;
    assert!(body.contains("conduit_conversations_total{outcome=\"barge_in\"} 1"), "{body}");
}

#[tokio::test]
async fn active_conversations_return_to_zero() {
    let body = scrape(a_turn()).await;
    assert!(body.contains("conduit_conversations_active 0"), "{body}");
}

#[tokio::test]
async fn an_unfinished_turn_is_still_counted_as_active() {
    let body = scrape(vec![Event::ConversationStarted]).await;
    assert!(body.contains("conduit_conversations_active 1"), "{body}");
}

#[tokio::test]
async fn tool_calls_are_counted_and_timed() {
    let call = ToolCallId::new("call_1");
    let body = scrape(vec![
        Event::ConversationStarted,
        Event::ToolRequested { call: call.clone(), name: "search".into() },
        Event::ToolStarted { call: call.clone() },
        Event::ToolCompleted { call: call.clone(), duration_ms: 250 },
        Event::ToolFailed { call, error: "boom".into() },
        Event::ConversationCompleted,
    ])
    .await;

    assert!(body.contains("conduit_tool_calls_total{outcome=\"completed\"} 1"), "{body}");
    assert!(body.contains("conduit_tool_calls_total{outcome=\"failed\"} 1"), "{body}");
    assert!(body.contains("conduit_tool_duration_seconds_count 1"), "{body}");
    // 250ms must land in the 0.25s bucket, not above it.
    assert!(body.contains("conduit_tool_duration_seconds_bucket{le=\"0.25\"} 1"), "{body}");
}

#[tokio::test]
async fn stage_failures_name_the_node_and_whether_it_recovered() {
    let body = scrape(vec![
        Event::ConversationStarted,
        Event::StageFailed { node: "stt".into(), error: "offline".into(), recovered: false },
        Event::ConversationCancelled { reason: CancelReason::Error },
    ])
    .await;

    assert!(
        body.contains("conduit_stage_failures_total{node=\"stt\",recovered=\"false\"} 1"),
        "{body}"
    );
}

#[tokio::test]
async fn token_usage_is_counted_by_direction() {
    let body = scrape(a_turn()).await;
    assert!(body.contains("conduit_llm_tokens_total{direction=\"prompt\"} 11"), "{body}");
    assert!(body.contains("conduit_llm_tokens_total{direction=\"completion\"} 3"), "{body}");
}

#[tokio::test]
async fn a_scrape_is_valid_exposition_even_when_nothing_has_happened() {
    let metrics = Metrics::new();
    let body = metrics.render();

    assert!(body.contains("# TYPE conduit_events_total counter"), "{body}");
    assert!(body.contains("# TYPE conduit_conversations_active gauge"), "{body}");
    assert!(body.contains("# TYPE conduit_turn_duration_seconds histogram"), "{body}");
    for line in body.lines() {
        assert!(
            line.starts_with('#') || line.contains(' '),
            "every sample line needs a value: {line}"
        );
    }
}
