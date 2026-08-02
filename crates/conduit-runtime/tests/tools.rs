//! Turns in which the model speaks, calls a tool, and speaks again.
//!
//! This is the shape a tool-using assistant actually has: "Sure, let me look
//! that up" — search — "it's sunny". The preamble is only worth saying if it
//! is spoken *while* the tool runs, so that is asserted directly.

mod fakes;

use std::time::Duration;

use conduit_core::bus::{EventBus, Subscription};
use conduit_core::event::{Event, UtteranceSegmentRole};
use conduit_core::graph::PipelineGraph;
use conduit_core::id::{SpeakerId, ToolCallId};
use conduit_core::testing::voice_graph;
use conduit_provider::llm::Role;
use conduit_provider::stt::Transcript;
use conduit_provider::tool::Permission;
use conduit_runtime::{Providers, Runner};
use fakes::{
    audio_of, stop, token, tool_call, wants_tools, Behaviour, FakeLlm, FakeStt, FakeTool,
    FakeTts,
};
use futures_util::StreamExt;

/// A pipeline with one tool available to the model.
fn graph_with_tool() -> PipelineGraph {
    voice_graph("tools").stt("fake-stt").core("fake-llm").tool("search").tts("fake-tts").build()
}

/// A model that speaks, calls `search`, then speaks again.
fn talkative_model(call: ToolCallId) -> FakeLlm {
    FakeLlm::scripted(vec![
        vec![
            token("Sure, let me look that up. "),
            tool_call(call.clone(), "search"),
            wants_tools(),
        ],
        vec![token("It is sunny."), stop()],
    ])
}

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

fn names(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            serde_json::to_value(event).expect("serialize")["type"]
                .as_str()
                .expect("tag")
                .to_owned()
        })
        .collect()
}

/// Runs one turn to completion, failing fast rather than hanging.
async fn run_turn(runner: &Runner) {
    let turn = async {
        let _: Vec<_> = runner.run(audio_of(&["a"])).speech().collect().await;
    };
    tokio::time::timeout(Duration::from_secs(5), turn).await.expect("turn completes");
}

#[tokio::test]
async fn speaks_before_and_after_a_tool_call() {
    let call = ToolCallId::new("call_abc123");
    let tts = FakeTts::new();
    let tool = FakeTool::new("search", serde_json::json!({ "forecast": "sunny" }));
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("what is the weather")]))
        .with_llm(talkative_model(call.clone()))
        .with_tool(tool.clone())
        .with_tts(tts.clone());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    assert_eq!(tool.invocations().len(), 1, "the tool must run");
    assert_eq!(tts.spoken(), ["Sure, let me look that up.", "It is sunny."]);
}

#[tokio::test]
async fn the_preamble_is_spoken_while_the_tool_runs() {
    // The tool blocks until speech starts. If the runtime waited for tools
    // before speaking, neither side could proceed and this would time out.
    let call = ToolCallId::new("call_abc123");
    let tts = FakeTts::new();
    let tool = FakeTool::new("search", serde_json::json!({}))
        .behaving(Behaviour::WaitFor(tts.spoke(), serde_json::json!({ "ok": true })));

    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(talkative_model(call.clone()))
        .with_tool(tool.clone())
        .with_tts(tts.clone());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    assert_eq!(tool.invocations().len(), 1);
    assert_eq!(tts.spoken(), ["Sure, let me look that up.", "It is sunny."]);
}

#[tokio::test]
async fn tool_turns_emit_reconstruction_boundary_events() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let call = ToolCallId::new("call_abc123");
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(FakeLlm::scripted(vec![
            vec![token("Let me check"), tool_call(call.clone(), "search"), wants_tools()],
            vec![token("It is sunny"), stop()],
        ]))
        .with_tool(FakeTool::new("search", serde_json::json!({ "forecast": "sunny" })))
        .with_tts(tts.clone());

    let runner =
        Runner::prepare(&graph_with_tool(), &providers, bus).expect("graph is executable");
    run_turn(&runner).await;

    let events = drain(&mut subscription).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::ToolBatchStarted { calls, model_round, .. }
                if calls == &vec![call.clone()] && *model_round == 1
        )),
        "expected a tool batch boundary: {:?}",
        names(&events)
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::UtteranceSegmentStarted {
                role: UtteranceSegmentRole::AssistantPreamble,
                text,
                ..
            } if text == "Let me check"
        )),
        "expected a preamble boundary: {:?}",
        names(&events)
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::UtteranceSegmentStarted {
                role: UtteranceSegmentRole::AssistantResponse,
                text,
                ..
            } if text == "It is sunny"
        )),
        "expected a final response boundary: {:?}",
        names(&events)
    );
}

#[tokio::test]
async fn an_unpunctuated_preamble_is_still_spoken() {
    let call = ToolCallId::new("call_abc123");
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        // No sentence-ending punctuation before the tool call.
        .with_llm(FakeLlm::scripted(vec![
            vec![token("Let me check"), tool_call(call.clone(), "search"), wants_tools()],
            vec![token("Sunny."), stop()],
        ]))
        .with_tool(FakeTool::new("search", serde_json::json!({})))
        .with_tts(tts.clone());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    assert_eq!(tts.spoken(), ["Let me check", "Sunny."]);
}

#[tokio::test]
async fn tool_spoken_output_is_spoken_before_the_model_continues() {
    let call = ToolCallId::new("call_abc123");
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("turn on the lamp")]))
        .with_llm(FakeLlm::scripted(vec![
            vec![token("One second. "), tool_call(call.clone(), "search"), wants_tools()],
            vec![token("All set."), stop()],
        ]))
        .with_tool(FakeTool::new("search", serde_json::json!({ "state": "on" })).behaving(
            Behaviour::Speak(
                serde_json::json!({ "state": "on" }),
                "The lamp is on.".to_owned(),
            ),
        ))
        .with_tts(tts.clone());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    assert_eq!(tts.spoken(), ["One second.", "The lamp is on.", "All set."]);
}

/// The result the model was given for `call`, in the round after it asked.
fn tool_result(llm: &FakeLlm, round: usize) -> String {
    llm.requests()[round]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("a tool result message")
        .content
        .clone()
}

#[tokio::test]
async fn a_tool_needing_confirmation_is_refused_rather_than_reported_done() {
    // The dangerous failure this guards against: a model told something
    // ambiguous about a lock or a purchase, deciding it succeeded, and saying
    // so. The result must read as a refusal to anything reading it.
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let call = ToolCallId::new("call_abc123");
    let llm = talkative_model(call.clone());
    let tts = FakeTts::new();
    let tool = FakeTool::new("search", serde_json::json!({}))
        .permitted(Permission::DenyUntilConfirmed { prompt: "Turn off the oven?".to_owned() });
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("turn off the oven")]))
        .with_llm(llm.clone())
        .with_tool(tool.clone())
        .with_tts(tts.clone());

    let runner =
        Runner::prepare(&graph_with_tool(), &providers, bus).expect("graph is executable");
    run_turn(&runner).await;

    assert!(tool.invocations().is_empty(), "a tool needing confirmation must not run");

    let result = tool_result(&llm, 1);
    assert!(result.contains("NOT run"), "the refusal must be unmissable: {result}");
    assert!(
        result.contains("Turn off the oven?"),
        "the model needs the prompt to explain itself: {result}"
    );

    // Nothing asks the speaker anything, because nothing could hear an answer.
    assert!(
        !tts.spoken().iter().any(|spoken| spoken.contains("Turn off the oven")),
        "an unanswerable question must not be spoken: {:?}",
        tts.spoken()
    );

    // Still observable: an operator watching the bus sees the tool was gated.
    let events = drain(&mut subscription).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::ToolConfirmationRequested { call: id, prompt } if *id == call && prompt == "Turn off the oven?"
        )),
        "expected a confirmation event: {:?}",
        names(&events)
    );
}

#[tokio::test]
async fn the_model_still_answers_after_a_confirmation_refusal() {
    // A refusal is a fact to work with, not the end of the turn.
    let call = ToolCallId::new("call_abc123");
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("turn off the oven")]))
        .with_llm(talkative_model(call.clone()))
        .with_tool(FakeTool::new("search", serde_json::json!({})).permitted(
            Permission::DenyUntilConfirmed { prompt: "Turn off the oven?".to_owned() },
        ))
        .with_tts(tts.clone());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    assert_eq!(tts.spoken(), ["Sure, let me look that up.", "It is sunny."]);
}

#[tokio::test]
async fn the_identified_speaker_reaches_the_tool() {
    // The seam a per-speaker tool policy needs. Nothing identifies a voice in
    // production yet, so the speaker is supplied here directly — this test is
    // what makes the path from a turn to a permission check real ahead of the
    // provider, rather than something to discover once one exists.
    let call = ToolCallId::new("call_abc123");
    let speaker = SpeakerId::new();
    let tool = FakeTool::new("search", serde_json::json!({}));
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(talkative_model(call))
        .with_tool(tool.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    let turn = async {
        let _: Vec<_> = runner.run_as(speaker, audio_of(&["a"])).speech().collect().await;
    };
    tokio::time::timeout(Duration::from_secs(5), turn).await.expect("turn completes");

    let contexts = tool.contexts();
    assert!(!contexts.is_empty(), "the tool must have been consulted");
    for context in &contexts {
        assert_eq!(
            context.speaker,
            Some(speaker),
            "every check and invocation must know who is speaking"
        );
    }
}

#[tokio::test]
async fn a_tool_sees_no_speaker_when_no_one_was_identified() {
    // Pins the documented current behaviour: threading the seam did not make
    // identification work, and a tool must keep deciding what an unknown voice
    // may do. A device or conversation standing in for a speaker here would
    // make every per-speaker policy silently wrong.
    let call = ToolCallId::new("call_abc123");
    let tool = FakeTool::new("search", serde_json::json!({}));
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(talkative_model(call))
        .with_tool(tool.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    let contexts = tool.contexts();
    assert!(!contexts.is_empty(), "the tool must have been consulted");
    assert!(
        contexts.iter().all(|context| context.speaker.is_none()),
        "nothing identifies a voice yet: {contexts:?}"
    );
}

#[tokio::test]
async fn the_tool_result_goes_back_to_the_model() {
    let call = ToolCallId::new("call_abc123");
    let llm = talkative_model(call.clone());
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(llm.clone())
        .with_tool(FakeTool::new("search", serde_json::json!({ "forecast": "sunny" })))
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    let requests = llm.requests();
    assert_eq!(requests.len(), 2, "the model must be called again with the result");

    // The model is told which tools it may call.
    assert_eq!(requests[0].tools.len(), 1);
    assert_eq!(requests[0].tools[0].name, "search");

    let second = &requests[1].messages;
    let result = second
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("a tool result message");
    assert_eq!(result.tool_call, Some(call.clone()));
    assert!(result.content.contains("sunny"), "unexpected result: {}", result.content);

    // The assistant's own preamble stays in the history.
    assert!(
        second.iter().any(|message| message.role == Role::Assistant
            && message.content.contains("look that up")),
        "the preamble must be preserved: {second:?}"
    );
}

#[tokio::test]
async fn the_lifecycle_of_a_tool_call_reaches_the_bus() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let call = ToolCallId::new("call_abc123");
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(talkative_model(call.clone()))
        .with_tool(FakeTool::new("search", serde_json::json!({})))
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&graph_with_tool(), &providers, bus).expect("graph is executable");
    run_turn(&runner).await;

    let events = drain(&mut subscription).await;
    let observed = names(&events);
    for expected in ["ToolRequested", "ToolStarted", "ToolCompleted"] {
        assert!(observed.contains(&expected.to_owned()), "missing {expected}: {observed:?}");
    }
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ToolRequested { call: id, name } if *id == call && name == "search"
    )));
}

#[tokio::test]
async fn a_failing_tool_is_reported_and_the_model_carries_on() {
    let bus = EventBus::default();
    let mut subscription = bus.subscribe();
    let call = ToolCallId::new("call_abc123");
    let llm = talkative_model(call.clone());
    let tts = FakeTts::new();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(llm.clone())
        .with_tool(
            FakeTool::new("search", serde_json::json!({}))
                .behaving(Behaviour::Fail("the service is down".to_owned())),
        )
        .with_tts(tts.clone());

    let runner =
        Runner::prepare(&graph_with_tool(), &providers, bus).expect("graph is executable");
    run_turn(&runner).await;

    let events = drain(&mut subscription).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolFailed { call: id, .. } if *id == call)),
        "expected ToolFailed: {:?}",
        names(&events)
    );

    // A failed tool is a fact for the model to work with, not the end of the
    // turn: it is told what went wrong and still gets to answer.
    let requests = llm.requests();
    assert_eq!(requests.len(), 2);
    let result = requests[1]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("a tool result message");
    assert!(result.content.contains("down"), "unexpected result: {}", result.content);
    assert_eq!(tts.spoken().last().map(String::as_str), Some("It is sunny."));
}

#[tokio::test]
async fn a_denied_tool_is_never_invoked() {
    let call = ToolCallId::new("call_abc123");
    let llm = talkative_model(call.clone());
    let tool = FakeTool::new("search", serde_json::json!({}))
        .permitted(Permission::Deny { reason: "not allowed in this room".to_owned() });
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(llm.clone())
        .with_tool(tool.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    assert!(tool.invocations().is_empty(), "a denied tool must not run");
    let result = llm.requests()[1]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("a tool result message")
        .content
        .clone();
    assert!(result.contains("not allowed"), "the model must be told why: {result}");
}

#[tokio::test]
async fn tools_requested_together_run_together() {
    let first = ToolCallId::new("call_one");
    let second = ToolCallId::new("call_two");
    let clock = FakeTool::new("clock", serde_json::json!({ "time": "noon" }));
    let search = FakeTool::new("search", serde_json::json!({ "forecast": "sunny" }));

    let graph = voice_graph("tools")
        .stt("fake-stt")
        .core("fake-llm")
        .tool("search")
        .tool("clock")
        .tts("fake-tts")
        .build();
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather and time")]))
        .with_llm(FakeLlm::scripted(vec![
            vec![
                token("One moment. "),
                tool_call(first.clone(), "search"),
                tool_call(second.clone(), "clock"),
                wants_tools(),
            ],
            vec![token("Sunny at noon."), stop()],
        ]))
        .with_tool(search.clone())
        .with_tool(clock.clone())
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&graph, &providers, EventBus::default()).expect("graph is executable");
    run_turn(&runner).await;

    assert_eq!(search.invocations().len(), 1);
    assert_eq!(clock.invocations().len(), 1);
}

#[tokio::test]
async fn an_unknown_tool_is_reported_rather_than_ignored() {
    let call = ToolCallId::new("call_abc123");
    let llm = FakeLlm::scripted(vec![
        vec![token("Checking. "), tool_call(call.clone(), "teleport"), wants_tools()],
        vec![token("Cannot do that."), stop()],
    ]);
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("teleport me")]))
        .with_llm(llm.clone())
        .with_tool(FakeTool::new("search", serde_json::json!({})))
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    let result = llm.requests()[1]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("a tool result message")
        .content
        .clone();
    assert!(result.contains("teleport"), "the model must be told: {result}");
}

#[tokio::test]
async fn a_model_that_never_stops_calling_tools_is_cut_off() {
    let call = ToolCallId::new("call_abc123");
    let tool = FakeTool::new("search", serde_json::json!({}));
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("loop forever")]))
        .with_llm(
            FakeLlm::scripted(vec![vec![
                token("Working. "),
                tool_call(call.clone(), "search"),
                wants_tools(),
            ]])
            .repeating(),
        )
        .with_tool(tool.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    run_turn(&runner).await;

    // Bounded, not infinite: the turn ends on its own.
    assert!(tool.invocations().len() <= 8, "tool rounds must be bounded");
    assert!(!tool.invocations().is_empty());
}

#[tokio::test]
async fn a_pipeline_without_tools_offers_the_model_none() {
    let llm = FakeLlm::new(vec!["Hello."]);
    let graph = voice_graph("plain").stt("fake-stt").core("fake-llm").tts("fake-tts").build();

    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("hi")]))
        .with_llm(llm.clone())
        .with_tts(FakeTts::new());

    let runner =
        Runner::prepare(&graph, &providers, EventBus::default()).expect("graph is executable");
    run_turn(&runner).await;

    assert!(llm.requests()[0].tools.is_empty());
}

#[tokio::test]
async fn a_bound_tool_must_name_a_registered_tool() {
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![]))
        .with_llm(FakeLlm::new(vec![]))
        .with_tts(FakeTts::new());

    let error = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect_err("the search tool is not registered");
    assert!(matches!(error, conduit_core::Error::UnknownProvider(name) if name == "search"));
}

#[tokio::test]
async fn authenticating_a_device_does_not_identify_a_speaker() {
    // Conflating the two would make every per-speaker tool policy wrong:
    // anyone who could reach the satellite would inherit whoever owns its
    // token. A device token says which box connected, and nothing more.
    let call = ToolCallId::new("call_abc123");
    let tool = FakeTool::new("search", serde_json::json!({}));
    let providers = Providers::new()
        .with_stt(FakeStt::new(vec![Transcript::final_text("weather")]))
        .with_llm(talkative_model(call))
        .with_tool(tool.clone())
        .with_tts(FakeTts::new());

    let runner = Runner::prepare(&graph_with_tool(), &providers, EventBus::default())
        .expect("graph is executable");
    let _: Vec<_> = runner
        .run_for_device(conduit_core::id::DeviceId::new(), audio_of(&["a"]))
        .speech()
        .collect()
        .await;

    let contexts = tool.contexts();
    assert!(!contexts.is_empty(), "the tool must have been consulted");
    assert!(
        contexts.iter().all(|context| context.speaker.is_none()),
        "a device token is not a voice print: {contexts:?}"
    );
}
