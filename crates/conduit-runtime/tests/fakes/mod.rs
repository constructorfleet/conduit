//! Recording provider doubles.
//!
//! Each fake returns canned output and remembers what it was asked to do, so
//! tests can assert on what actually reached a stage.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use conduit_core::event::FinishReason;
use conduit_core::id::ToolCallId;
use conduit_core::{Error, Result};
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel, ToolSpec, Usage};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::tool::{Permission, Tool, ToolContext, ToolOutput};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Provider};
use futures_util::StreamExt;
use tokio::sync::Notify;

/// Builds an audio stream whose chunks carry the given payloads.
pub fn audio_of(chunks: &[&str]) -> ChunkStream<AudioChunk> {
    let chunks: Vec<_> = chunks
        .iter()
        .enumerate()
        .map(|(sequence, data)| {
            Ok(AudioChunk {
                sequence: sequence as u64,
                data: Bytes::copy_from_slice(data.as_bytes()),
            })
        })
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

/// Wraps canned items into a stream.
fn stream_of<T: Send + 'static>(items: Vec<T>) -> ChunkStream<T> {
    Box::pin(futures_util::stream::iter(items.into_iter().map(Ok)))
}

/// A recognizer that replays a fixed list of transcripts.
#[derive(Clone)]
pub struct FakeStt {
    transcripts: Vec<Transcript>,
}

impl FakeStt {
    pub fn new(transcripts: Vec<Transcript>) -> Self {
        Self { transcripts }
    }
}

impl Provider for FakeStt {
    fn name(&self) -> &str {
        "fake-stt"
    }
}

#[async_trait::async_trait]
impl SpeechToText for FakeStt {
    async fn transcribe(
        &self,
        audio: ChunkStream<AudioChunk>,
        _options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>> {
        // Drain the input so a runtime that never forwards audio fails here.
        let received = audio.count().await;
        assert!(received > 0, "the recognizer was given no audio");
        Ok(stream_of(self.transcripts.clone()))
    }
}

/// A recognizer that always fails.
#[derive(Clone)]
pub struct FailingStt;

impl Provider for FailingStt {
    fn name(&self) -> &str {
        "fake-stt"
    }
}

#[async_trait::async_trait]
impl SpeechToText for FailingStt {
    async fn transcribe(
        &self,
        _audio: ChunkStream<AudioChunk>,
        _options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>> {
        Err(Error::Config("recognizer is offline".to_owned()))
    }
}

/// A text delta.
pub fn token(delta: &str) -> Completion {
    Completion::Token { delta: delta.to_owned() }
}

/// A request to run `name`.
pub fn tool_call(id: ToolCallId, name: &str) -> Completion {
    Completion::ToolCall {
        id,
        name: name.to_owned(),
        arguments: serde_json::json!({ "query": "weather" }),
    }
}

/// The end of a round in which the model wants tools run.
pub fn wants_tools() -> Completion {
    Completion::Finished { reason: FinishReason::ToolUse, usage: Usage::default() }
}

/// The end of a round in which the model is done.
pub fn stop() -> Completion {
    Completion::Finished { reason: FinishReason::Stop, usage: Usage::default() }
}

/// A model that replays a script, one round per `complete` call.
///
/// A tool-calling turn takes several rounds: the model asks for a tool, the
/// runtime answers with the result, and the model is called again. Each entry
/// in the script is one of those rounds.
#[derive(Clone)]
pub struct FakeLlm {
    rounds: Arc<Mutex<Vec<Vec<Completion>>>>,
    /// Whether the final round replays forever once the script runs out, so a
    /// test can drive a model that never stops asking for tools.
    repeat_last: bool,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl FakeLlm {
    /// A model that always emits `tokens` and stops, however often it is
    /// called.
    pub fn new(tokens: Vec<&str>) -> Self {
        let mut round: Vec<Completion> = tokens.into_iter().map(token).collect();
        round.push(stop());
        Self::scripted(vec![round]).repeating()
    }

    /// A model that replays `rounds` in order.
    pub fn scripted(rounds: Vec<Vec<Completion>>) -> Self {
        Self {
            rounds: Arc::new(Mutex::new(rounds)),
            repeat_last: false,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Replays the final round forever once the script is exhausted.
    pub fn repeating(mut self) -> Self {
        self.repeat_last = true;
        self
    }

    /// Every request this model received, in order.
    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

impl Provider for FakeLlm {
    fn name(&self) -> &str {
        "fake-llm"
    }
}

#[async_trait::async_trait]
impl LanguageModel for FakeLlm {
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream<Completion>> {
        self.requests.lock().expect("lock").push(request);
        let mut rounds = self.rounds.lock().expect("lock");
        let round = if rounds.len() > 1 || !self.repeat_last {
            if rounds.is_empty() {
                Vec::new()
            } else {
                rounds.remove(0)
            }
        } else {
            rounds.first().cloned().unwrap_or_default()
        };
        Ok(stream_of(round))
    }

    fn supports_tools(&self) -> bool {
        true
    }
}

/// A synthesizer that emits the requested text as bytes and records it.
#[derive(Clone)]
pub struct FakeTts {
    spoken: Arc<Mutex<Vec<String>>>,
    spoke: Arc<Notify>,
}

impl FakeTts {
    pub fn new() -> Self {
        Self { spoken: Arc::new(Mutex::new(Vec::new())), spoke: Arc::new(Notify::new()) }
    }

    /// The text of every synthesis request, in order.
    pub fn spoken(&self) -> Vec<String> {
        self.spoken.lock().expect("lock").clone()
    }

    /// Notified the first time anything is synthesized.
    ///
    /// Lets a test make a tool wait for speech to start, which deadlocks if
    /// the runtime speaks only after tools finish.
    pub fn spoke(&self) -> Arc<Notify> {
        Arc::clone(&self.spoke)
    }
}

impl Provider for FakeTts {
    fn name(&self) -> &str {
        "fake-tts"
    }
}

#[async_trait::async_trait]
impl TextToSpeech for FakeTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        self.spoken.lock().expect("lock").push(request.text.clone());
        self.spoke.notify_one();
        Ok(stream_of(vec![SpeechChunk {
            sequence: 0,
            format: request.format,
            data: Bytes::from(request.text.into_bytes()),
        }]))
    }

    async fn voices(&self) -> Result<Vec<Voice>> {
        Ok(vec![Voice {
            id: "fake".to_owned(),
            name: "Fake".to_owned(),
            language: "en-US".to_owned(),
        }])
    }
}

/// How a fake tool behaves when invoked.
#[derive(Clone)]
pub enum Behaviour {
    /// Return a value.
    Succeed(serde_json::Value),
    /// Return a value with text the runtime should speak directly.
    Speak(serde_json::Value, String),
    /// Fail with a message.
    Fail(String),
    /// Wait for a signal, then return a value. Used to prove that speech and
    /// tool execution overlap rather than taking turns.
    WaitFor(Arc<Notify>, serde_json::Value),
}

/// A tool that records its invocations.
#[derive(Clone)]
pub struct FakeTool {
    name: &'static str,
    behaviour: Behaviour,
    permission: Permission,
    invocations: Arc<Mutex<Vec<serde_json::Value>>>,
    /// The context of every invocation and every permission check, so a test
    /// can assert on who the runtime said was speaking. Permission checks are
    /// recorded too: a denied tool never reaches `invoke`, and the whole point
    /// of the speaker is deciding that denial.
    contexts: Arc<Mutex<Vec<ToolContext>>>,
}

impl FakeTool {
    /// A tool that succeeds with `value`.
    pub fn new(name: &'static str, value: serde_json::Value) -> Self {
        Self {
            name,
            behaviour: Behaviour::Succeed(value),
            permission: Permission::Allow,
            invocations: Arc::new(Mutex::new(Vec::new())),
            contexts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Replaces the tool's behaviour.
    pub fn behaving(mut self, behaviour: Behaviour) -> Self {
        self.behaviour = behaviour;
        self
    }

    /// Replaces the tool's permission decision.
    pub fn permitted(mut self, permission: Permission) -> Self {
        self.permission = permission;
        self
    }

    /// The arguments of every invocation, in order.
    pub fn invocations(&self) -> Vec<serde_json::Value> {
        self.invocations.lock().expect("lock").clone()
    }

    /// The context of every permission check and invocation, in order.
    pub fn contexts(&self) -> Vec<ToolContext> {
        self.contexts.lock().expect("lock").clone()
    }
}

impl Provider for FakeTool {
    fn name(&self) -> &str {
        self.name
    }
}

#[async_trait::async_trait]
impl Tool for FakeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.to_owned(),
            description: format!("the {} tool", self.name),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    async fn permission(
        &self,
        _arguments: &serde_json::Value,
        context: &ToolContext,
    ) -> Permission {
        self.contexts.lock().expect("lock").push(context.clone());
        self.permission.clone()
    }

    async fn invoke(
        &self,
        arguments: serde_json::Value,
        context: ToolContext,
    ) -> Result<ToolOutput> {
        self.invocations.lock().expect("lock").push(arguments);
        self.contexts.lock().expect("lock").push(context);
        match &self.behaviour {
            Behaviour::Succeed(value) => Ok(ToolOutput::new(value.clone())),
            Behaviour::Speak(value, spoken) => {
                Ok(ToolOutput::new(value.clone()).with_spoken(spoken))
            }
            Behaviour::Fail(message) => Err(Error::Config(message.clone())),
            Behaviour::WaitFor(notify, value) => {
                notify.notified().await;
                Ok(ToolOutput::new(value.clone()))
            }
        }
    }
}
