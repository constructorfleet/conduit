//! Recording provider doubles.
//!
//! Each fake returns canned output and remembers what it was asked to do, so
//! tests can assert on what actually reached a stage.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
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

/// Builds an audio stream of `count` chunks of `bytes` zeroed samples each.
///
/// Payload size is what the capture events report, so a test asserting on a
/// duration needs to control it rather than count characters in a string.
pub fn audio_of_size(count: usize, bytes: usize) -> ChunkStream<AudioChunk> {
    let chunks: Vec<_> = (0..count)
        .map(|sequence| {
            Ok(AudioChunk { sequence: sequence as u64, data: Bytes::from(vec![0_u8; bytes]) })
        })
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

/// Builds an audio stream that yields `before` chunks and then fails.
///
/// A microphone can stop mid-utterance, and what the capture stage reports in
/// that case is the interesting part: an operator needs to see capture end
/// rather than watch a stream that simply stops describing itself.
pub fn audio_failing_after(before: usize) -> ChunkStream<AudioChunk> {
    let mut items: Vec<Result<AudioChunk>> = (0..before)
        .map(|sequence| {
            Ok(AudioChunk { sequence: sequence as u64, data: Bytes::from_static(b"aa") })
        })
        .collect();
    items.push(Err(Error::Config("the microphone stopped".to_owned())));
    Box::pin(futures_util::stream::iter(items))
}

/// Wraps canned items into a stream.
fn stream_of<T: Send + 'static>(items: Vec<T>) -> ChunkStream<T> {
    Box::pin(futures_util::stream::iter(items.into_iter().map(Ok)))
}

/// A recognizer that replays a fixed list of transcripts.
#[derive(Clone)]
pub struct FakeStt {
    transcripts: Vec<Transcript>,
    encodings: Vec<Encoding>,
    /// Whether being handed no audio at all is a test failure.
    demands_audio: bool,
}

impl FakeStt {
    pub fn new(transcripts: Vec<Transcript>) -> Self {
        Self { transcripts, encodings: Vec::new(), demands_audio: true }
    }

    pub fn accepting_encodings(mut self, encodings: &[Encoding]) -> Self {
        self.encodings = encodings.to_vec();
        self
    }

    /// Stops asserting that audio arrived.
    ///
    /// The assertion is normally what catches a runtime that never forwards
    /// what it captured, so this is only for the tests whose subject *is* an
    /// utterance with no audio in it.
    pub fn accepting_silence(mut self) -> Self {
        self.demands_audio = false;
        self
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
        assert!(received > 0 || !self.demands_audio, "the recognizer was given no audio");
        Ok(stream_of(self.transcripts.clone()))
    }

    fn supports_encoding(&self, encoding: Encoding) -> bool {
        self.encodings.is_empty() || self.encodings.contains(&encoding)
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

/// A recognizer that accepts the audio and never answers.
///
/// The failure the idle deadline exists for, and the one nothing else catches: a
/// provider that errors ends the turn through `StageFailed`, while this one
/// leaves it waiting with no error to report and no stage having failed.
#[derive(Clone, Default)]
pub struct SilentStt;

impl Provider for SilentStt {
    fn name(&self) -> &str {
        "fake-stt"
    }
}

#[async_trait::async_trait]
impl SpeechToText for SilentStt {
    async fn transcribe(
        &self,
        audio: ChunkStream<AudioChunk>,
        _options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>> {
        // Drained first so the capture events are published: the turn must have
        // made progress and *then* stopped, rather than never having started.
        let _ = audio.count().await;
        std::future::pending().await
    }
}

/// A model that accepts the request and never answers.
///
/// Stalls a turn that got as far as a transcript, which is the case where the
/// stage a timeout names is the only way to tell which provider is at fault.
#[derive(Clone, Default)]
pub struct SilentLlm;

impl Provider for SilentLlm {
    fn name(&self) -> &str {
        "fake-llm"
    }
}

#[async_trait::async_trait]
impl LanguageModel for SilentLlm {
    fn models(&self) -> &[String] {
        static MODELS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        MODELS.get_or_init(|| vec!["fake-model".to_owned()])
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<ChunkStream<Completion>> {
        std::future::pending().await
    }

    fn supports_tools(&self) -> bool {
        true
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
    models: Vec<String>,
    system_prompt: Option<String>,
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
            // A real provider advertises what it serves, and resolution now
            // refuses a pipeline where nothing names a model. Fakes advertise
            // one so a test about something else does not have to.
            models: vec!["fake-model".to_owned()],
            system_prompt: None,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Replays the final round forever once the script is exhausted.
    pub fn repeating(mut self) -> Self {
        self.repeat_last = true;
        self
    }

    /// Advertises a finite model catalogue.
    pub fn serving(mut self, models: &[&str]) -> Self {
        self.models = models.iter().map(|model| (*model).to_owned()).collect();
        self
    }

    /// Stands in for a provider definition carrying a system prompt.
    pub fn with_system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt = Some(prompt.to_owned());
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

    fn models(&self) -> &[String] {
        &self.models
    }

    fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }
}

/// A synthesizer that emits the requested text as bytes and records it.
#[derive(Clone)]
pub struct FakeTts {
    spoken: Arc<Mutex<Vec<String>>>,
    voices: Arc<Mutex<Vec<Option<String>>>>,
    spoke: Arc<Notify>,
    encodings: Vec<Encoding>,
    /// Rate this synthesizer speaks at, when it is not the requested one.
    native_rate: Option<u32>,
}

impl FakeTts {
    pub fn new() -> Self {
        Self {
            spoken: Arc::new(Mutex::new(Vec::new())),
            voices: Arc::new(Mutex::new(Vec::new())),
            spoke: Arc::new(Notify::new()),
            encodings: Vec::new(),
            native_rate: None,
        }
    }

    /// Speaks at `sample_rate` regardless of what was asked for, as a real
    /// voice trained at one rate does. Emits exactly one second of audio.
    pub fn speaking_at(mut self, sample_rate: u32) -> Self {
        self.native_rate = Some(sample_rate);
        self
    }

    pub fn producing_encodings(mut self, encodings: &[Encoding]) -> Self {
        self.encodings = encodings.to_vec();
        self
    }

    /// The text of every synthesis request, in order.
    pub fn spoken(&self) -> Vec<String> {
        self.spoken.lock().expect("lock").clone()
    }

    /// The voice asked for on every synthesis request, in order.
    pub fn voices_requested(&self) -> Vec<Option<String>> {
        self.voices.lock().expect("lock").clone()
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
        if let Some(sample_rate) = self.native_rate {
            let format = AudioFormat { sample_rate, ..request.format };
            return Ok(stream_of(vec![SpeechChunk {
                sequence: 0,
                format,
                data: Bytes::from(vec![0_u8; sample_rate as usize * 2]),
            }]));
        }
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

    fn supports_encoding(&self, encoding: Encoding) -> bool {
        self.encodings.is_empty() || self.encodings.contains(&encoding)
    }
}

/// A synthesizer that starts and then never finishes.
///
/// Lets a test interrupt a turn that is genuinely mid-reply: with a
/// synthesizer that returns instantly, a turn ends before a stop could
/// plausibly arrive, and the test would prove nothing about interrupting.
#[derive(Clone)]
pub struct HangingTts {
    speaking: Arc<Notify>,
}

impl HangingTts {
    pub fn new() -> Self {
        Self { speaking: Arc::new(Notify::new()) }
    }

    /// Notified once synthesis has been asked for.
    ///
    /// Holds a permit if nobody is waiting yet, so a test that reaches this
    /// after synthesis began still sees it rather than waiting forever.
    pub fn speaking(&self) -> Arc<Notify> {
        Arc::clone(&self.speaking)
    }
}

impl Provider for HangingTts {
    fn name(&self) -> &str {
        "fake-tts"
    }
}

#[async_trait::async_trait]
impl TextToSpeech for HangingTts {
    async fn synthesize(&self, _request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        self.speaking.notify_one();
        std::future::pending().await
    }

    async fn voices(&self) -> Result<Vec<Voice>> {
        Ok(Vec::new())
    }
}

/// A synthesizer that emits one byte at a time, slowly.
///
/// Lets a test act while a turn is mid-reply. [`FakeTts`] returns a whole
/// sentence instantly and the output channel buffers several, so a turn using it
/// can finish before a test's next line runs — which would make an assertion
/// about interrupting pass or fail on timing.
#[derive(Clone, Default)]
pub struct SlowTts;

impl Provider for SlowTts {
    fn name(&self) -> &str {
        "fake-tts"
    }
}

#[async_trait::async_trait]
impl TextToSpeech for SlowTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        let format = request.format;
        Ok(Box::pin(futures_util::stream::unfold(
            request.text.into_bytes().into_iter().enumerate(),
            move |mut bytes| async move {
                let (sequence, byte) = bytes.next()?;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let chunk = SpeechChunk {
                    sequence: sequence as u64,
                    format,
                    data: Bytes::from(vec![byte]),
                };
                Some((Ok(chunk), bytes))
            },
        )))
    }

    async fn voices(&self) -> Result<Vec<Voice>> {
        Ok(Vec::new())
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

/// A memory store that keeps everything in a vector.
///
/// Records what it was asked to store and what it was searched for, so a test
/// can assert that a turn read before answering and wrote after.
#[derive(Clone, Default)]
pub struct FakeMemory {
    stored: Arc<Mutex<Vec<conduit_provider::memory::Record>>>,
    searched: Arc<Mutex<Vec<conduit_provider::memory::Query>>>,
    /// Returned from every search, whatever was asked.
    recalls: Arc<Mutex<Vec<String>>>,
}

impl FakeMemory {
    /// A store that remembers nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that answers every search with `content`.
    pub fn recalling(content: &str) -> Self {
        let memory = Self::new();
        memory.recalls.lock().expect("lock").push(content.to_owned());
        memory
    }

    /// Everything stored, in order.
    pub fn stored(&self) -> Vec<conduit_provider::memory::Record> {
        self.stored.lock().expect("lock").clone()
    }

    /// Every search, in order.
    pub fn searched(&self) -> Vec<conduit_provider::memory::Query> {
        self.searched.lock().expect("lock").clone()
    }
}

impl Provider for FakeMemory {
    fn name(&self) -> &str {
        "fake-memory"
    }
}

#[async_trait::async_trait]
impl conduit_provider::memory::Memory for FakeMemory {
    async fn store(&self, record: conduit_provider::memory::Record) -> Result<()> {
        self.stored.lock().expect("lock").push(record);
        Ok(())
    }

    async fn search(
        &self,
        query: conduit_provider::memory::Query,
    ) -> Result<Vec<conduit_provider::memory::Match>> {
        self.searched.lock().expect("lock").push(query);
        Ok(self
            .recalls
            .lock()
            .expect("lock")
            .iter()
            .map(|content| conduit_provider::memory::Match {
                record: conduit_provider::memory::Record {
                    content: content.clone(),
                    scope: conduit_core::memory::Scope::Conversation,
                    conversation: None,
                    speaker: None,
                    metadata: serde_json::Value::Null,
                },
                score: 1.0,
            })
            .collect())
    }

    async fn forget_conversation(
        &self,
        _conversation: conduit_core::id::ConversationId,
    ) -> Result<()> {
        Ok(())
    }
}

/// A wake word detector that accepts once it has heard `after` chunks.
///
/// The lag is the point: a real detector reports an activation after the
/// phrase has ended, and a gate that assumed otherwise would clip whatever was
/// said next.
pub struct FakeWake {
    after: usize,
    accept: bool,
    heard: Arc<Mutex<usize>>,
}

impl FakeWake {
    /// A detector that accepts on the `after`th chunk it is given.
    pub fn accepting_after(after: usize) -> Self {
        Self { after, accept: true, heard: Arc::new(Mutex::new(0)) }
    }

    /// A detector that scores every chunk below its threshold.
    pub fn never_accepting() -> Self {
        Self { after: usize::MAX, accept: false, heard: Arc::new(Mutex::new(0)) }
    }

    /// How much audio the detector was given, which is every chunk captured —
    /// a gate that only fed it until it woke would never hear the next
    /// activation.
    pub fn heard(&self) -> usize {
        *self.heard.lock().expect("lock")
    }
}

#[async_trait::async_trait]
impl Provider for FakeWake {
    fn name(&self) -> &str {
        "fake-wake"
    }
}

#[async_trait::async_trait]
impl conduit_provider::wake::WakeWordDetector for FakeWake {
    async fn detect(
        &self,
        audio: ChunkStream<AudioChunk>,
        _phrases: Vec<conduit_provider::wake::WakePhrase>,
    ) -> Result<ChunkStream<conduit_provider::wake::Detection>> {
        let after = self.after;
        let accept = self.accept;
        let heard = Arc::clone(&self.heard);
        Ok(Box::pin(audio.enumerate().map(move |(index, _)| {
            *heard.lock().expect("lock") += 1;
            Ok(conduit_provider::wake::Detection {
                phrase: "hey jarvis".to_owned(),
                confidence: if accept && index + 1 >= after { 0.9 } else { 0.1 },
                accepted: accept && index + 1 >= after,
            })
        })))
    }

    fn configured_phrases(&self) -> Vec<conduit_provider::wake::WakePhrase> {
        vec![conduit_provider::wake::WakePhrase::new("hey jarvis")]
    }
}

/// A speaker identifier that always answers with the same voice.
pub struct FakeSpeaker {
    speaker: Option<conduit_core::id::SpeakerId>,
    failing: bool,
    heard: Arc<Mutex<usize>>,
}

impl FakeSpeaker {
    /// An identifier that recognizes every voice as `speaker`.
    pub fn knowing(speaker: conduit_core::id::SpeakerId) -> Self {
        Self { speaker: Some(speaker), failing: false, heard: Arc::new(Mutex::new(0)) }
    }

    /// An identifier whose service is down.
    pub fn unreachable() -> Self {
        Self { speaker: None, failing: true, heard: Arc::new(Mutex::new(0)) }
    }

    /// How many bytes of audio identification was given.
    pub fn heard(&self) -> usize {
        *self.heard.lock().expect("lock")
    }
}

#[async_trait::async_trait]
impl Provider for FakeSpeaker {
    fn name(&self) -> &str {
        "fake-speaker"
    }
}

#[async_trait::async_trait]
impl conduit_provider::speaker::SpeakerIdentifier for FakeSpeaker {
    async fn identify(
        &self,
        mut audio: ChunkStream<AudioChunk>,
    ) -> Result<conduit_provider::speaker::Identification> {
        while let Some(chunk) = audio.next().await {
            *self.heard.lock().expect("lock") += chunk?.data.len();
        }
        if self.failing {
            return Err(Error::Config("speaker service is down".to_owned()));
        }
        Ok(conduit_provider::speaker::Identification {
            speaker: self.speaker,
            confidence: 0.95,
        })
    }

    async fn enroll(
        &self,
        _speaker: conduit_core::id::SpeakerId,
        _samples: ChunkStream<AudioChunk>,
    ) -> Result<()> {
        Ok(())
    }

    async fn forget(&self, _speaker: conduit_core::id::SpeakerId) -> Result<()> {
        Ok(())
    }
}

/// A transform that rewrites every segment the same way and records what it
/// was given.
#[derive(Clone)]
pub struct FakeTransform {
    name: String,
    /// What every segment becomes, or `None` to pass it through unchanged.
    replacement: Option<String>,
    /// Text to strip out of every segment, when it appears.
    remove: Option<String>,
    seen: Arc<Mutex<Vec<String>>>,
    failing: bool,
}

impl FakeTransform {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            replacement: None,
            remove: None,
            seen: Arc::new(Mutex::new(Vec::new())),
            failing: false,
        }
    }

    /// Rewrites every segment to `text`.
    pub fn replacing_with(mut self, text: &str) -> Self {
        self.replacement = Some(text.to_owned());
        self
    }

    /// Removes every occurrence of `text`.
    pub fn removing(mut self, text: &str) -> Self {
        self.remove = Some(text.to_owned());
        self
    }

    /// Refuses every segment, as a transform whose backend is down would.
    pub fn failing(mut self) -> Self {
        self.failing = true;
        self
    }

    /// Every segment this transform was given, in order.
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }
}

impl Provider for FakeTransform {
    fn name(&self) -> &str {
        &self.name
    }
}

#[async_trait::async_trait]
impl conduit_provider::transform::UtteranceTransform for FakeTransform {
    async fn transform(&self, segment: &str) -> Result<String> {
        self.seen.lock().expect("lock").push(segment.to_owned());
        if self.failing {
            return Err(Error::Config("the rewriting service is down".to_owned()));
        }
        if let Some(replacement) = &self.replacement {
            return Ok(replacement.clone());
        }
        match &self.remove {
            Some(text) => Ok(segment.replace(text, "").trim().to_owned()),
            None => Ok(segment.to_owned()),
        }
    }
}
