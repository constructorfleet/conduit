//! What happens to a turn that stops getting anywhere.
//!
//! The failure being tested is a provider that accepts a request and never
//! answers. It is not a failure any error path catches: nothing returns an
//! error, no stage fails, and the turn simply waits. Before there was a
//! deadline, it waited for as long as the client stayed connected — and since a
//! turn is reachable only through the audio it produces, a wedged turn was
//! indistinguishable from a thoughtful one.
//!
//! Every test here sets a deadline in milliseconds. The real default is
//! generous, and a test that waited for it would take a minute to tell us
//! something a short one tells us immediately.

mod fakes;

use std::time::Duration;

use conduit_core::bus::{EventBus, Subscription};
use conduit_core::event::{CancelReason, Event};
use conduit_core::graph::PipelineGraph;
use conduit_core::testing::voice_graph;
use conduit_core::Error;
use conduit_provider::stt::Transcript;
use conduit_runtime::{Providers, Runner, DEFAULT_IDLE_TIMEOUT};
use fakes::{audio_of, FakeLlm, FakeStt, FakeTts, HangingTts, SilentLlm, SilentStt};
use futures_util::StreamExt;

/// stt -> llm -> tts, the shape the runtime can execute.
fn linear_graph() -> PipelineGraph {
    voice_graph("deadline").stt("fake-stt").llm("fake-llm").tts("fake-tts").build()
}

/// A deadline short enough that a stalled turn ends within a test's patience.
const BRIEF: Option<Duration> = Some(Duration::from_millis(50));

/// Long enough that nothing working reaches it, short enough to fail fast.
const PATIENT: Option<Duration> = Some(Duration::from_secs(10));

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

/// The reason a turn's cancellation was published with, if it was cancelled.
fn cancelled_with(events: &[Event]) -> Option<CancelReason> {
    events.iter().find_map(|event| match event {
        Event::ConversationCancelled { reason } => Some(*reason),
        _ => None,
    })
}

#[tokio::test]
async fn a_recognizer_that_never_answers_ends_the_turn() {
    // The whole point: this used to hang until the client gave up.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(SilentStt)
        .with_llm(FakeLlm::new(vec!["unused"]))
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_idle_timeout(BRIEF);

    let spoken: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        runner.run(audio_of(&["a"])).speech().collect(),
    )
    .await
    .expect("the turn ends rather than hanging");

    let events = drain(&mut subscription).await;
    assert_eq!(
        cancelled_with(&events),
        Some(CancelReason::IdleTimeout),
        "a stalled turn must be reported as one, not as an error or an interruption"
    );
    assert!(
        spoken.iter().any(|chunk| matches!(chunk, Err(Error::Timeout { .. }))),
        "the caller must be told why the reply stopped"
    );
}

#[tokio::test]
async fn a_model_that_never_answers_ends_the_turn() {
    // A different stage stalling, to show the deadline is not specific to one.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(SilentLlm)
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_idle_timeout(BRIEF);

    let _: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        runner.run(audio_of(&["a"])).speech().collect(),
    )
    .await
    .expect("the turn ends rather than hanging");

    let events = drain(&mut subscription).await;
    assert_eq!(cancelled_with(&events), Some(CancelReason::IdleTimeout));
}

#[tokio::test]
async fn a_synthesizer_that_never_answers_ends_the_turn() {
    // The stage where a stall is worst: the person has already been listening
    // to silence for a while by the time anything notices.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(HangingTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_idle_timeout(BRIEF);

    let _: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        runner.run(audio_of(&["a"])).speech().collect(),
    )
    .await
    .expect("the turn ends rather than hanging");

    let events = drain(&mut subscription).await;
    assert_eq!(cancelled_with(&events), Some(CancelReason::IdleTimeout));
}

#[tokio::test]
async fn the_timeout_names_the_stage_that_went_quiet() {
    // With four providers in a pipeline, "the turn timed out" leaves an operator
    // guessing which one to look at.
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(SilentLlm)
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable")
        .with_idle_timeout(BRIEF);

    let spoken: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        runner.run(audio_of(&["a"])).speech().collect(),
    )
    .await
    .expect("the turn ends");

    let failure = spoken
        .into_iter()
        .find_map(|chunk| chunk.err())
        .expect("the caller is told the turn timed out");
    let message = failure.to_string();
    // `LlmRequestStarted` is published before the model is called, so the last
    // thing this turn reported names the stage that then went quiet — which is
    // the provider an operator needs to go and look at.
    assert!(
        message.contains("reasoning"),
        "the message must name where progress stopped: {message}"
    );
    assert!(matches!(failure, Error::Timeout { .. }), "{failure:?}");
}

#[tokio::test]
async fn a_turn_that_keeps_talking_is_never_abandoned() {
    // The property that makes a deadline safe to have on by default: a long
    // reply is not a stalled one. `SlowTts` emits a byte every 20ms, so this
    // turn is continuously slower than the 50ms deadline yet always working.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["One. ", "Two. ", "Three. ", "Four. ", "Five."]))
        .with_tts(fakes::SlowTts);
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_idle_timeout(BRIEF);

    let started = std::time::Instant::now();
    let spoken: Vec<_> = tokio::time::timeout(
        Duration::from_secs(10),
        runner.run(audio_of(&["a"])).speech().collect(),
    )
    .await
    .expect("the turn finishes");
    let took = started.elapsed();

    // Each sentence is trimmed before synthesis, so the spoken audio has no
    // separators — every word the model produced is here.
    let audio: Vec<u8> = spoken
        .into_iter()
        .map(|chunk| chunk.expect("no chunk may fail"))
        .flat_map(|chunk| chunk.data.to_vec())
        .collect();
    assert_eq!(
        String::from_utf8(audio).expect("utf-8"),
        "One.Two.Three.Four.Five.",
        "the whole reply must survive a deadline shorter than the reply took"
    );
    // Otherwise a turn that finished quickly would pass this without ever
    // testing whether a slow one is left alone.
    assert!(
        took > BRIEF.expect("a deadline is set"),
        "the reply must have outlasted its own deadline to prove anything, took {took:?}"
    );

    let events = drain(&mut subscription).await;
    assert_eq!(cancelled_with(&events), None, "nothing was cancelled");
    assert!(
        events.iter().any(|event| matches!(event, Event::ConversationCompleted)),
        "the turn completed: {events:?}"
    );
}

#[tokio::test]
async fn the_deadline_can_be_removed() {
    // A deployment with its own deadline above the runtime may want this, so it
    // must be expressible — and must genuinely not fire.
    let bus = EventBus::default();
    let providers = Providers::new()
        .with_stt(SilentStt)
        .with_llm(FakeLlm::new(vec!["unused"]))
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_idle_timeout(None);

    let mut conversation = runner.run(audio_of(&["a"]));
    // Read as the raw reply stream rather than through `speech`, which
    // consumes the conversation this test still needs to abort.
    let waited =
        tokio::time::timeout(Duration::from_millis(200), conversation.output.next()).await;

    assert!(waited.is_err(), "with no deadline, a stalled turn keeps waiting");
    conversation.abort();
}

#[tokio::test]
async fn an_explicit_stop_is_reported_as_asked_for_even_when_time_is_up() {
    // Both races can be ready at once. A client that pressed the button did
    // press it, and `idle_timeout` would blame a provider for a person's choice.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let providers = Providers::new()
        .with_stt(SilentStt)
        .with_llm(FakeLlm::new(vec!["unused"]))
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, bus)
        .expect("graph is executable")
        .with_idle_timeout(Some(Duration::ZERO));

    let conversation = runner.run(audio_of(&["a"]));
    conversation.stop.request();

    let events = drain(&mut subscription).await;
    assert_eq!(
        cancelled_with(&events),
        Some(CancelReason::UserRequested),
        "a stop outranks a deadline: {events:?}"
    );
}

#[tokio::test]
async fn a_caller_can_wait_for_a_turn_to_finish() {
    // A turn used to be spawned and forgotten, so nothing could tell whether
    // anyone was mid-sentence — which is what a shutdown needs to know.
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable")
        .with_idle_timeout(PATIENT);

    let mut conversation = runner.run(audio_of(&["a"]));
    // Drained first: the output channel is bounded, so a turn nobody reads from
    // is a turn that has not finished.
    while conversation.output.next().await.is_some() {}

    tokio::time::timeout(Duration::from_secs(5), conversation.finished())
        .await
        .expect("the turn ends")
        .expect("and was neither aborted nor panicked");
}

#[tokio::test]
async fn a_caller_can_abort_a_turn_that_would_never_end() {
    // The shutdown path: no deadline, a provider that never answers, and a
    // process that has to exit anyway.
    let providers = Providers::new()
        .with_stt(SilentStt)
        .with_llm(FakeLlm::new(vec!["unused"]))
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable")
        .with_idle_timeout(None);

    let conversation = runner.run(audio_of(&["a"]));
    conversation.abort();

    let outcome = tokio::time::timeout(Duration::from_secs(5), conversation.finished())
        .await
        .expect("an aborted turn ends");
    assert!(outcome.is_err_and(|error| error.is_cancelled()), "it was aborted");
}

#[tokio::test]
async fn a_turn_is_bounded_without_anyone_configuring_one() {
    // The default matters more than its value: a deployment that configures
    // nothing must not be the one that hangs.
    assert!(DEFAULT_IDLE_TIMEOUT > Duration::ZERO);
    assert!(
        DEFAULT_IDLE_TIMEOUT >= Duration::from_secs(30),
        "a tight default would abandon a slow local model that is working fine"
    );

    // And a runner nobody configured has it, which is what the tests above
    // override rather than introduce.
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(FakeLlm::new(vec!["Hello."]))
        .with_tts(FakeTts::new());
    let runner = Runner::prepare(&linear_graph(), &providers, EventBus::default())
        .expect("graph is executable");

    let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;
}
