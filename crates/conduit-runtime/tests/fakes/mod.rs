//! Recording provider doubles.
//!
//! Each fake returns canned output and remembers what it was asked to do, so
//! tests can assert on what actually reached a stage.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use conduit_core::event::FinishReason;
use conduit_core::{Error, Result};
use conduit_provider::llm::{Completion, CompletionRequest, LanguageModel, Usage};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Provider};
use futures_util::StreamExt;

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

/// A model that replays fixed token deltas and records its requests.
#[derive(Clone)]
pub struct FakeLlm {
    tokens: Vec<String>,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl FakeLlm {
    pub fn new(tokens: Vec<&str>) -> Self {
        Self {
            tokens: tokens.into_iter().map(str::to_owned).collect(),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every request this model received.
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
        let mut items: Vec<Completion> = self
            .tokens
            .iter()
            .map(|delta| Completion::Token { delta: delta.clone() })
            .collect();
        items
            .push(Completion::Finished { reason: FinishReason::Stop, usage: Usage::default() });
        Ok(stream_of(items))
    }
}

/// A synthesizer that emits the requested text as bytes and records it.
#[derive(Clone)]
pub struct FakeTts {
    spoken: Arc<Mutex<Vec<String>>>,
}

impl FakeTts {
    pub fn new() -> Self {
        Self { spoken: Arc::new(Mutex::new(Vec::new())) }
    }

    /// The text of every synthesis request, in order.
    pub fn spoken(&self) -> Vec<String> {
        self.spoken.lock().expect("lock").clone()
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
