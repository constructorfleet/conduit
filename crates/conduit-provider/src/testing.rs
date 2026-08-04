//! In-memory providers for development and tests.
//!
//! These let a whole pipeline run with no speech engine, no model server, and
//! no network — useful for exercising the transport, the event stream, and the
//! graph editor before any real provider is configured.
//!
//! They are not toys pretending to be engines: each one treats audio as UTF-8
//! text, so "audio" in a test is just a readable string. Nothing here belongs
//! in a deployment that expects to hear speech.

use bytes::Bytes;
use conduit_core::audio::AudioFormat;
use conduit_core::event::FinishReason;
use conduit_core::Result;
use futures_util::StreamExt;

use crate::descriptor::{Descriptor, Metadata};
use crate::llm::{Completion, CompletionRequest, LanguageModel, Role, Usage};
use crate::registry::Capability;
use crate::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use crate::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use crate::{ChunkStream, Provider};

/// Wraps canned items into a stream.
fn stream_of<T: Send + 'static>(items: Vec<T>) -> ChunkStream<T> {
    Box::pin(futures_util::stream::iter(items.into_iter().map(Ok)))
}

/// A recognizer that reads the audio bytes as UTF-8 text.
///
/// "Speaking" to a pipeline built on this means sending the words as bytes.
#[derive(Debug, Clone, Default)]
pub struct EchoStt;

impl Provider for EchoStt {
    fn descriptor(&self) -> &Descriptor {
        static DESCRIPTOR: std::sync::OnceLock<Descriptor> = std::sync::OnceLock::new();
        DESCRIPTOR.get_or_init(|| {
            Descriptor::new("echo-stt", Capability::Stt).with_label("Echo recognizer")
        })
    }
}

#[async_trait::async_trait]
impl SpeechToText for EchoStt {
    async fn transcribe(
        &self,
        mut audio: ChunkStream<AudioChunk>,
        _options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>> {
        let mut text = String::new();
        while let Some(chunk) = audio.next().await {
            let chunk = chunk?;
            text.push_str(&String::from_utf8_lossy(&chunk.data));
        }
        Ok(stream_of(vec![Transcript::final_text(text.trim())]))
    }
}

/// A model that repeats what it heard, one word at a time.
///
/// Streaming word by word keeps it useful for checking that speech starts
/// before generation ends.
#[derive(Debug, Clone, Default)]
pub struct EchoLlm;

impl Provider for EchoLlm {
    /// Advertises one model, as a real provider does.
    ///
    /// Resolution refuses a pipeline where neither the node nor the provider
    /// names a model, since there would be nothing to ask for.
    fn descriptor(&self) -> &Descriptor {
        static DESCRIPTOR: std::sync::OnceLock<Descriptor> = std::sync::OnceLock::new();
        DESCRIPTOR.get_or_init(|| {
            Descriptor::new("echo-llm", Capability::Llm)
                .with_label("Echo model")
                .with_metadata(Metadata::default().with_models(vec!["echo-model".to_owned()]))
        })
    }
}

#[async_trait::async_trait]
impl LanguageModel for EchoLlm {
    async fn complete(&self, request: CompletionRequest) -> Result<ChunkStream<Completion>> {
        let heard = request
            .messages
            .iter()
            .rfind(|message| message.role == Role::User)
            .map_or_else(String::new, |message| message.content.clone());

        let reply = format!("You said: {heard}.");
        let mut items: Vec<Completion> = reply
            .split_inclusive(' ')
            .map(|word| Completion::Token { delta: word.to_owned() })
            .collect();
        items
            .push(Completion::Finished { reason: FinishReason::Stop, usage: Usage::default() });
        Ok(stream_of(items))
    }
}

/// A synthesizer that emits the text as UTF-8 bytes.
#[derive(Debug, Clone, Default)]
pub struct EchoTts;

impl Provider for EchoTts {
    fn descriptor(&self) -> &Descriptor {
        static DESCRIPTOR: std::sync::OnceLock<Descriptor> = std::sync::OnceLock::new();
        DESCRIPTOR.get_or_init(|| {
            Descriptor::new("echo-tts", Capability::Tts)
                .with_label("Echo synthesizer")
                .with_metadata(Metadata::default().with_voices(vec![Voice {
                    id: "echo".to_owned(),
                    name: "Echo".to_owned(),
                    language: "en-US".to_owned(),
                }]))
        })
    }
}

#[async_trait::async_trait]
impl TextToSpeech for EchoTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        Ok(stream_of(vec![SpeechChunk {
            sequence: 0,
            format: request.format,
            data: Bytes::from(request.text.into_bytes()),
        }]))
    }
}

/// Builds an audio stream from text, for talking to [`EchoStt`].
#[must_use]
pub fn spoken(text: &str) -> ChunkStream<AudioChunk> {
    stream_of(vec![AudioChunk { sequence: 0, data: Bytes::from(text.to_owned().into_bytes()) }])
}

/// The format these providers work in, which is the pipeline default.
#[must_use]
pub const fn format() -> AudioFormat {
    AudioFormat::DEFAULT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Message;

    #[tokio::test]
    async fn the_recognizer_reads_audio_as_text() {
        let mut transcripts = EchoStt
            .transcribe(spoken("turn on the light"), TranscribeOptions::default())
            .await
            .expect("transcribes");
        let transcript = transcripts.next().await.expect("one").expect("ok");
        assert_eq!(transcript.text, "turn on the light");
        assert!(transcript.is_final);
    }

    #[tokio::test]
    async fn the_model_repeats_the_last_thing_said() {
        let request = CompletionRequest::new(
            "echo",
            vec![Message::system("ignored"), Message::user("hello there")],
        );
        let items: Vec<_> = EchoLlm
            .complete(request)
            .await
            .expect("completes")
            .map(|item| item.expect("ok"))
            .collect()
            .await;

        let spoken: String = items
            .iter()
            .filter_map(|item| match item {
                Completion::Token { delta } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(spoken, "You said: hello there.");
        assert!(matches!(items.last(), Some(Completion::Finished { .. })));
    }

    #[tokio::test]
    async fn the_synthesizer_emits_the_text() {
        let mut chunks =
            EchoTts.synthesize(SynthesisRequest::new("hello")).await.expect("synthesizes");
        let chunk = chunks.next().await.expect("one").expect("ok");
        assert_eq!(&chunk.data[..], b"hello");
    }
}
