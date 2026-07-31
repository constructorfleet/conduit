//! What a scrape says after a pipeline has run.

use std::sync::Arc;

use conduit_core::bus::EventBus;
use conduit_core::event::{CancelReason, Envelope, Event, FinishReason};
use conduit_core::id::{ConversationId, ToolCallId, TraceId};
use conduit_metrics::collector::MAX_TRACKED;
use conduit_metrics::{Collector, Metrics};

/// Publishes `events` for one conversation and returns the resulting scrape.
///
/// Dropping the bus ends the collector's loop once it has drained everything
/// buffered, so the scrape can be awaited rather than slept for.
async fn scrape(events: Vec<Event>) -> String {
    let bus = EventBus::default();
    let metrics = Arc::new(Metrics::new());
    let collector = Collector::spawn(Arc::clone(&metrics), &bus);

    let conversation = ConversationId::new();
    let trace = TraceId::new();
    for event in events {
        bus.publish(Envelope::new(trace, event).with_conversation(conversation));
    }

    // The collector is a separate task; let it finish before scraping.
    drop(bus);
    collector.await.expect("collector task");
    metrics.render()
}

/// Feeds events straight to a collector and returns the resulting scrape.
///
/// Some situations cannot be staged through a bus — a conversation that ends
/// without the collector ever seeing it start, or four thousand simultaneous
/// turns — so those tests drive [`Collector::record`] directly.
fn collect(events: Vec<(Option<ConversationId>, Event)>) -> String {
    let metrics = Arc::new(Metrics::new());
    let mut collector = Collector::new(Arc::clone(&metrics));
    for (conversation, event) in events {
        collector.record(conversation, &event);
    }
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
async fn an_end_without_a_start_does_not_skew_active_conversations() {
    // A conversation the collector never saw begin — evicted from tracking, or
    // started before the collector subscribed — must not decrement a gauge it
    // never incremented, or every later reading is wrong by one.
    let started = ConversationId::new();
    let never_seen = ConversationId::new();
    let body = collect(vec![
        (Some(started), Event::ConversationStarted),
        (Some(never_seen), Event::ConversationCompleted),
        (Some(never_seen), Event::ConversationCancelled { reason: CancelReason::BargeIn }),
    ]);

    assert!(body.contains("conduit_conversations_active 1"), "{body}");
    // The outcome is still real volume, even with the start missing.
    assert!(body.contains("conduit_conversations_total{outcome=\"completed\"} 1"), "{body}");
}

#[tokio::test]
async fn tracking_full_forgets_the_oldest_conversation() {
    let starts: Vec<(Option<ConversationId>, Event)> = (0..=MAX_TRACKED)
        .map(|_| (Some(ConversationId::new()), Event::ConversationStarted))
        .collect();
    let oldest = starts[0].0.expect("conversation");

    let body = collect(starts.clone());
    assert!(body.contains("conduit_conversations_forgotten_total 1"), "{body}");
    // The gauge reports what is tracked, so it cannot exceed the tracking limit
    // and cannot drift away from the conversations it is derived from.
    assert!(body.contains(&format!("conduit_conversations_active {MAX_TRACKED}")), "{body}");

    // The forgotten conversation still ends eventually. Eviction already
    // released its slot, so its late end must not release a second one.
    let mut with_late_end = starts;
    with_late_end.push((Some(oldest), Event::ConversationCompleted));
    let body = collect(with_late_end);
    assert!(body.contains(&format!("conduit_conversations_active {MAX_TRACKED}")), "{body}");
}

#[tokio::test]
async fn nothing_is_forgotten_while_tracking_has_room() {
    let body = scrape(a_turn()).await;
    assert!(body.contains("conduit_conversations_forgotten_total 0"), "{body}");
}

#[tokio::test]
async fn every_cancel_reason_has_its_own_outcome_label() {
    // `IdleTimeout` and `Shutdown` are not published by the runtime yet, so the
    // mapping is exercised where it lives rather than through a whole turn.
    for (reason, outcome) in [
        (CancelReason::BargeIn, "barge_in"),
        (CancelReason::IdleTimeout, "idle_timeout"),
        (CancelReason::UserRequested, "user_requested"),
        (CancelReason::Error, "error"),
        (CancelReason::Shutdown, "shutdown"),
    ] {
        let conversation = ConversationId::new();
        let body = collect(vec![
            (Some(conversation), Event::ConversationStarted),
            (Some(conversation), Event::ConversationCancelled { reason }),
        ]);
        assert!(
            body.contains(&format!("conduit_conversations_total{{outcome=\"{outcome}\"}} 1")),
            "{reason:?} should be labelled {outcome}: {body}"
        );
        assert!(
            body.contains(&format!(
                "conduit_turn_duration_seconds_count{{outcome=\"{outcome}\"}} 1"
            )),
            "{reason:?} should time the turn: {body}"
        );
    }
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
async fn requested_tool_calls_are_counted_separately_from_their_outcomes() {
    // A call that is requested and never resolves — a hung provider — is only
    // visible as the gap between requests and outcomes, so requests need their
    // own series. It is a separate metric rather than another `outcome` label
    // so that summing `conduit_tool_calls_total` keeps meaning "calls that
    // finished" instead of double counting every call.
    let hung = ToolCallId::new("call_hung");
    let done = ToolCallId::new("call_done");
    let body = scrape(vec![
        Event::ConversationStarted,
        Event::ToolRequested { call: hung, name: "search".into() },
        Event::ToolRequested { call: done.clone(), name: "clock".into() },
        Event::ToolCompleted { call: done, duration_ms: 10 },
    ])
    .await;

    assert!(body.contains("conduit_tool_calls_requested_total 2"), "{body}");
    assert!(body.contains("conduit_tool_calls_total{outcome=\"completed\"} 1"), "{body}");
    assert!(!body.contains("conduit_tool_calls_total{outcome=\"requested\"}"), "{body}");
}

#[tokio::test]
async fn a_tool_call_awaiting_confirmation_is_an_outcome() {
    // The runtime answers the model and stops when a tool needs confirmation,
    // so this is where the call ends unless a human resumes it.
    let call = ToolCallId::new("call_1");
    let body = scrape(vec![
        Event::ConversationStarted,
        Event::ToolRequested { call: call.clone(), name: "unlock".into() },
        Event::ToolConfirmationRequested { call, prompt: "unlock the door?".into() },
    ])
    .await;

    assert!(
        body.contains("conduit_tool_calls_total{outcome=\"awaiting_confirmation\"} 1"),
        "{body}"
    );
}

#[tokio::test]
async fn events_lost_to_a_lagging_collector_are_counted() {
    // The bus drops the oldest events rather than stalling the audio path. The
    // collector owns its subscription, so it can report its own losses without
    // anything in the pipeline knowing metrics exist.
    let bus = EventBus::new(2);
    let subscription = bus.subscribe();
    let metrics = Arc::new(Metrics::new());

    let trace = TraceId::new();
    for _ in 0..6 {
        bus.publish(Envelope::new(trace, Event::ConversationStarted));
    }
    drop(bus);

    Collector::new(Arc::clone(&metrics)).run(subscription).await;
    let body = metrics.render();

    assert!(body.contains("conduit_events_dropped_total{subscriber=\"metrics\"} 4"), "{body}");
}

#[tokio::test]
async fn a_collector_that_keeps_up_reports_no_drops() {
    let body = scrape(a_turn()).await;
    assert!(body.contains("conduit_events_dropped_total{subscriber=\"metrics\"} 0"), "{body}");
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
    // Health signals read zero when idle rather than vanishing, so a dashboard
    // can tell "nothing has gone wrong" from "this metric does not exist".
    assert!(body.contains("conduit_conversations_active 0"), "{body}");
    assert!(body.contains("conduit_conversations_forgotten_total 0"), "{body}");
    assert!(body.contains("conduit_events_dropped_total{subscriber=\"metrics\"} 0"), "{body}");
    assert!(body.contains("# TYPE conduit_turn_duration_seconds histogram"), "{body}");
    for line in body.lines() {
        assert!(
            line.starts_with('#') || line.contains(' '),
            "every sample line needs a value: {line}"
        );
    }
}
