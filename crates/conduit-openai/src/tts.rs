//! Text-to-speech over the audio speech API.
//!
//! OpenAI serves this endpoint, and so do local shims such as
//! `openedai-speech`, which is how a Piper voice ends up reachable through the
//! same interface.

use conduit_core::audio::Encoding;
use conduit_core::{Error, Result};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::StreamExt;
use serde::Serialize;

use crate::failure::Failure;
use crate::http::Http;
use crate::OpenAiConfig;

/// The default voice when a pipeline names none.
const DEFAULT_VOICE: &str = "alloy";

/// Voices the hosted API offers. Local servers advertise their own, which is
/// why this is a fallback rather than a fixed list.
const HOSTED_VOICES: [&str; 6] = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"];

/// A synthesis request in the vendor's shape.
#[derive(Debug, Serialize)]
struct Request {
    model: String,
    input: String,
    voice: String,
    response_format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
}

/// A synthesizer served over the audio speech API.
#[derive(Debug, Clone)]
pub struct OpenAiTts {
    http: Http,
    model: String,
    voices: Vec<Voice>,
}

impl OpenAiTts {
    /// Builds a synthesizer using `model`, e.g. `"tts-1"`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built.
    pub fn new(config: &OpenAiConfig, model: impl Into<String>) -> Result<Self> {
        Ok(Self {
            http: Http::new(config)?,
            model: model.into(),
            voices: HOSTED_VOICES
                .iter()
                .map(|id| Voice {
                    id: (*id).to_owned(),
                    name: (*id).to_owned(),
                    language: "en-US".to_owned(),
                })
                .collect(),
        })
    }

    /// Replaces the advertised voice catalogue.
    ///
    /// A local server has its own voices, and nothing about this endpoint lets
    /// a client discover them.
    #[must_use]
    pub fn with_voices(mut self, voices: Vec<Voice>) -> Self {
        self.voices = voices;
        self
    }

    /// The voice to speak with, most specific choice first.
    ///
    /// A pipeline that names one gets it. Otherwise the provider definition's
    /// own first voice is what the operator configured, and speaking as the
    /// hosted default instead would ignore the only voice they asked for —
    /// which on a local `openedai-speech` server is not even a voice it has.
    fn voice_for(&self, requested: Option<String>) -> String {
        requested
            .or_else(|| self.voices.first().map(|voice| voice.id.clone()))
            .unwrap_or_else(|| DEFAULT_VOICE.to_owned())
    }
}

/// The `response_format` value for an encoding.
///
/// # Errors
///
/// Returns [`Error::Config`] for float PCM, which the API does not produce.
fn response_format(encoding: Encoding) -> Result<&'static str> {
    match encoding {
        Encoding::PcmS16Le => Ok("pcm"),
        Encoding::Flac => Ok("flac"),
        Encoding::Opus => Ok("opus"),
        Encoding::PcmF32Le => Err(Error::Config(
            "the speech API does not produce 32-bit float PCM; ask for PcmS16Le".to_owned(),
        )),
        // `Encoding` is non-exhaustive; a format this code predates is one the
        // server was never asked for.
        other => Err(Error::Config(format!("the speech API does not produce {other:?}"))),
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiTts {
    fn name(&self) -> &str {
        self.http.name()
    }

    async fn health(&self) -> Health {
        match self.http.send(self.http.get("models")).await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl TextToSpeech for OpenAiTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        let body = Request {
            model: self.model.clone(),
            input: request.text,
            voice: self.voice_for(request.voice),
            response_format: response_format(request.format.encoding)?,
            speed: request.rate,
        };
        tracing::debug!(model = %body.model, voice = %body.voice, "synthesizing");

        let response = self.http.send(self.http.post("audio/speech").json(&body)).await?;

        // The endpoint sends raw audio with no framing, so a chunk is however
        // much has arrived. Forwarding them as they land is what lets playback
        // start before synthesis finishes.
        let name = self.http.name().to_owned();
        let format = request.format;
        let mut sequence = 0_u64;
        let chunks = response.bytes_stream().map(move |packet| match packet {
            Ok(data) => {
                let chunk = SpeechChunk { sequence, format, data };
                sequence += 1;
                Ok(chunk)
            }
            // Audio that stops arriving partway through is classified like any
            // other transport failure, so a caller can tell a stalled server
            // from a rejected request.
            Err(error) => Err(Error::provider(&name, Failure::transport(&error))),
        });

        Ok(Box::pin(chunks))
    }

    async fn voices(&self) -> Result<Vec<Voice>> {
        Ok(self.voices.clone())
    }

    fn supports_encoding(&self, encoding: Encoding) -> bool {
        matches!(encoding, Encoding::PcmS16Le | Encoding::Flac | Encoding::Opus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::audio::AudioFormat;

    #[test]
    fn encodings_map_onto_response_formats() {
        assert_eq!(response_format(Encoding::PcmS16Le).expect("supported"), "pcm");
        assert_eq!(response_format(Encoding::Flac).expect("supported"), "flac");
        assert_eq!(response_format(Encoding::Opus).expect("supported"), "opus");
    }

    #[test]
    fn float_pcm_is_refused_with_an_actionable_message() {
        let error = response_format(Encoding::PcmF32Le).expect_err("unsupported");
        assert!(error.to_string().contains("PcmS16Le"), "{error}");
    }

    #[test]
    fn the_default_audio_format_is_supported() {
        assert!(response_format(AudioFormat::DEFAULT.encoding).is_ok());
    }

    fn synthesizer() -> OpenAiTts {
        OpenAiTts::new(&OpenAiConfig::default(), "tts-1").expect("client")
    }

    fn voice(id: &str) -> Voice {
        Voice { id: id.to_owned(), name: id.to_owned(), language: "en-US".to_owned() }
    }

    #[test]
    fn a_requested_voice_wins() {
        let provider = synthesizer().with_voices(vec![voice("shimmer")]);
        assert_eq!(provider.voice_for(Some("echo".to_owned())), "echo");
    }

    #[test]
    fn the_configured_voice_is_used_when_the_pipeline_names_none() {
        // The operator put a voice on the provider definition; speaking as
        // `alloy` anyway ignores the only voice they asked for.
        let provider = synthesizer().with_voices(vec![voice("shimmer"), voice("echo")]);
        assert_eq!(provider.voice_for(None), "shimmer");
    }

    #[test]
    fn the_hosted_default_remains_the_last_resort() {
        assert_eq!(synthesizer().with_voices(Vec::new()).voice_for(None), DEFAULT_VOICE);
    }
}
