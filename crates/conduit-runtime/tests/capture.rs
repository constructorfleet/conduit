//! What the capture stage reports about audio on its way to the recognizer.
//!
//! These events are the only description of the microphone anyone gets: the
//! samples themselves go to the recognizer and never to the bus. So the useful
//! questions are whether a turn says capture began, says it per chunk as the
//! audio flows rather than after the fact, and says it ended — including when
//! it ended badly.

mod fakes;

use std::time::Duration;

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::bus::{EventBus, Filter, Subscription};
use conduit_core::event::{Event, Stage};
use conduit_core::graph::{Edge, Node, PipelineGraph};
use conduit_provider::stt::Transcript;
use conduit_runtime::{Providers, Runner};
use fakes::{audio_failing_after, audio_of, audio_of_size, FakeLlm, FakeStt, FakeTts};
use futures_util::StreamExt;

/// stt -> llm -> tts, the shape the runtime can execute.
fn linear_graph() -> PipelineGraph {
    PipelineGraph::new("capture")
        .with_node(Node::stt("stt", "fake-stt"))
        .with_node(Node::llm("llm", "fake-llm"))
        .with_node(Node::tts("tts", "fake-tts"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
}

fn providers() -> Providers {
    Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new())
}

/// Reads events until the turn ends, so assertions see the whole sequence.
async fn drain(subscription: &mut Subscription) -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_secs(5), subscription.recv()).await
    {
        let terminal = envelope.event.is_terminal();
        events.push(envelope.event.clone());
        if terminal {
            break;
        }
    }
    events
}

#[tokio::test]
async fn a_turn_reports_the_audio_it_captured() {
    // The whole point of the stage: `?stages=capture` used to be a valid
    // subscription to a stream that could never say anything.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe_filtered(Filter::all().stages([Stage::Capture]));
    let runner =
        Runner::prepare(&linear_graph(), &providers(), bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of(&["one", "two", "three"])).speech().collect().await;

    // Filtered to one stage, so the terminal event never arrives to end the
    // drain; read exactly what capture should have produced.
    let mut events = Vec::new();
    for _ in 0..5 {
        let received = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
            .await
            .expect("capture events arrive")
            .expect("bus is alive");
        events.push(received.event.clone());
    }

    assert!(matches!(events[0], Event::AudioStarted { .. }), "{:?}", events[0]);
    assert!(matches!(events[1], Event::AudioChunkReceived { sequence: 0, bytes: 3 }));
    assert!(matches!(events[2], Event::AudioChunkReceived { sequence: 1, bytes: 3 }));
    assert!(matches!(events[3], Event::AudioChunkReceived { sequence: 2, bytes: 5 }));
    assert!(matches!(events[4], Event::AudioFinished { .. }), "{:?}", events[4]);
}

#[tokio::test]
async fn capture_events_arrive_while_the_audio_is_still_flowing() {
    // The distinction that makes this observability rather than a summary: a
    // turn that described its capture after the recognizer finished would
    // publish the same events, in the same order, uselessly late.
    //
    // The recognizer here drains the whole stream before returning a
    // transcript, so every chunk event landing before `SpeechFinal` is what
    // says they were published as the audio passed rather than at the end.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let runner =
        Runner::prepare(&linear_graph(), &providers(), bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of(&["a", "b"])).speech().collect().await;

    let events = drain(&mut subscription).await;
    let position = |wanted: &str| {
        events
            .iter()
            .position(|event| serde_json::to_value(event).expect("serialize")["type"] == wanted)
            .unwrap_or_else(|| panic!("expected {wanted} in {events:?}"))
    };

    assert!(
        position("AudioStarted") < position("AudioFinished"),
        "capture must open before it closes"
    );
    assert!(
        position("AudioFinished") < position("SpeechFinal"),
        "capture must be reported before the transcript it produced"
    );
}

#[tokio::test]
async fn the_reported_duration_comes_from_the_captured_bytes() {
    // One second of 16 kHz mono s16 is 32 000 bytes. A duration that did not
    // follow the audio would make "how long do people talk for?" unanswerable.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe_filtered(Filter::all().stages([Stage::Capture]));
    let runner =
        Runner::prepare(&linear_graph(), &providers(), bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of_size(2, 16_000)).speech().collect().await;

    let finished = capture_finished(&mut subscription).await;
    assert_eq!(finished, Some(1_000), "32 000 bytes of the default format is one second");
}

#[tokio::test]
async fn a_compressed_stream_reports_an_unknown_duration_rather_than_a_wrong_one() {
    // Opus bitrate is variable, so bytes say nothing about time. Deriving a
    // duration from them anyway would put a fabricated number on a dashboard.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe_filtered(Filter::all().stages([Stage::Capture]));
    let providers = Providers::new()
        .with_stt(
            FakeStt::new(vec![Transcript::final_text("hi")])
                .accepting_encodings(&[Encoding::Opus]),
        )
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new().producing_encodings(&[Encoding::Opus]));
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_format(AudioFormat { encoding: Encoding::Opus, ..AudioFormat::DEFAULT })
        .expect("the providers handle opus");

    let _: Vec<_> = runner.run(audio_of_size(2, 16_000)).speech().collect().await;

    let finished = capture_finished(&mut subscription).await;
    assert_eq!(finished, Some(0), "an unknown duration is reported as zero, not invented");
}

#[tokio::test]
async fn the_format_reported_is_the_one_being_captured() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe_filtered(Filter::all().stages([Stage::Capture]));
    let providers = Providers::new()
        .with_stt(
            FakeStt::new(vec![Transcript::final_text("hi")])
                .accepting_encodings(&[Encoding::PcmF32Le]),
        )
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new().producing_encodings(&[Encoding::PcmF32Le]));
    let captured =
        AudioFormat { encoding: Encoding::PcmF32Le, sample_rate: 8_000, channels: 1 };
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_format(captured)
        .expect("the providers handle 8 kHz f32");

    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    let started = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
        .await
        .expect("AudioStarted arrives")
        .expect("bus is alive");
    assert_eq!(
        started.event,
        Event::AudioStarted { format: captured },
        "a subscriber cannot interpret the byte counts without the real format"
    );
}

#[tokio::test]
async fn capture_that_fails_mid_utterance_is_still_reported_as_finished() {
    // A microphone that dies leaves the stream ending early. Reporting only
    // `AudioStarted` would leave a subscriber holding a capture that never
    // closes, which is the shape of a stuck pipeline rather than a failed one.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let runner =
        Runner::prepare(&linear_graph(), &providers(), bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_failing_after(2)).speech().collect().await;

    let events = drain(&mut subscription).await;
    let names: Vec<String> = events
        .iter()
        .map(|event| {
            serde_json::to_value(event).expect("serialize")["type"]
                .as_str()
                .expect("tag")
                .to_owned()
        })
        .collect();
    assert!(names.contains(&"AudioStarted".to_owned()), "{names:?}");
    assert!(names.contains(&"AudioFinished".to_owned()), "capture must close: {names:?}");
}

#[tokio::test]
async fn a_failed_chunk_is_not_counted_as_captured_audio() {
    // The error is not audio, so it must not be described as a chunk: a byte
    // count that included it would report more captured than was ever spoken.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe_filtered(Filter::all().stages([Stage::Capture]));
    let runner =
        Runner::prepare(&linear_graph(), &providers(), bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_failing_after(2)).speech().collect().await;

    let mut chunks = 0;
    for _ in 0..4 {
        let Ok(Some(received)) =
            tokio::time::timeout(Duration::from_secs(5), subscription.recv()).await
        else {
            break;
        };
        if matches!(received.event, Event::AudioChunkReceived { .. }) {
            chunks += 1;
        }
        if matches!(received.event, Event::AudioFinished { .. }) {
            break;
        }
    }
    assert_eq!(chunks, 2, "two chunks arrived; the failure is not a third");
}

#[tokio::test]
async fn a_turn_that_captures_nothing_reports_no_capture() {
    // A device that connects and sends nothing has not started capturing. An
    // `AudioStarted`/`AudioFinished` pair around no audio would make every
    // silent connection look like a zero-length utterance.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("")]).accepting_silence())
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());
    let runner =
        Runner::prepare(&linear_graph(), &providers, bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of(&[])).speech().collect().await;

    let events = drain(&mut subscription).await;
    assert!(
        !events.iter().any(|event| event.stage() == Stage::Capture),
        "nothing was captured, so nothing may be reported: {events:?}"
    );
}

#[tokio::test]
async fn a_turn_names_itself_before_doing_anything() {
    // `TurnStarted` is what lets a subscriber attribute the events that follow
    // to one turn rather than to whatever else the conversation contains.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let runner =
        Runner::prepare(&linear_graph(), &providers(), bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;

    let events = drain(&mut subscription).await;
    assert_eq!(events[0], Event::ConversationStarted);
    assert!(
        matches!(events[1], Event::TurnStarted { .. }),
        "the turn must be named before its work: {:?}",
        events[1]
    );
}

#[tokio::test]
async fn each_turn_gets_its_own_turn_id() {
    // A turn id that repeated would make two utterances indistinguishable in
    // whatever is grouping events by it.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let runner =
        Runner::prepare(&linear_graph(), &providers(), bus).expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;
    let first = drain(&mut subscription).await;
    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;
    let second = drain(&mut subscription).await;

    let turn_id = |events: &[Event]| {
        events.iter().find_map(|event| match event {
            Event::TurnStarted { turn } => Some(*turn),
            _ => None,
        })
    };
    let (first, second) = (turn_id(&first), turn_id(&second));
    assert!(first.is_some() && second.is_some(), "both turns must name themselves");
    assert_ne!(first, second, "a turn id must not be reused");
}

/// The duration reported by the turn's `AudioFinished`, if it published one.
async fn capture_finished(subscription: &mut Subscription) -> Option<u64> {
    for _ in 0..16 {
        let received =
            tokio::time::timeout(Duration::from_secs(5), subscription.recv()).await.ok()??;
        if let Event::AudioFinished { duration_ms } = received.event {
            return Some(duration_ms);
        }
    }
    None
}
