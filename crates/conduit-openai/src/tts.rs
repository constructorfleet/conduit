//! Text-to-speech over the audio speech API.
//!
//! OpenAI serves this endpoint, and so do local shims such as
//! `openedai-speech`, which is how a Piper voice ends up reachable through the
//! same interface.

use conduit_core::audio::Encoding;
use conduit_core::{Error, Result};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};
use futures_util::StreamExt;
use serde::Serialize;

use crate::OpenAiConfig;
use conduit_http::Failure;
use conduit_http::Http;

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
    /// Provider-specific settings, already checked against this provider's
    /// declared schema.
    #[serde(flatten)]
    settings: serde_json::Map<String, serde_json::Value>,
}

/// A synthesizer served over the audio speech API.
#[derive(Debug, Clone)]
pub struct OpenAiTts {
    http: Http,
    model: String,
    descriptor: Descriptor,
    default_settings: serde_json::Map<String, serde_json::Value>,
}

/// The encodings the speech endpoint produces.
const ENCODINGS: [Encoding; 3] = [Encoding::PcmS16Le, Encoding::Flac, Encoding::Opus];

/// What the speech endpoint accepts beyond a voice, a format, and a rate.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "instructions": {
                "type": "string",
                "description": "How to say it — accent, emotion, pacing — where the model supports it.",
            },
        },
    }))
    .expect("a literal object schema")
}

impl OpenAiTts {
    /// Builds a synthesizer using `model`, e.g. `"tts-1"`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built.
    pub fn new(config: &OpenAiConfig, model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        let voices = HOSTED_VOICES
            .iter()
            .map(|id| Voice {
                id: (*id).to_owned(),
                name: (*id).to_owned(),
                language: "en-US".to_owned(),
            })
            .collect();
        let descriptor = config
            .descriptor(Capability::Tts)
            .with_metadata(
                Metadata::default()
                    .with_models(vec![model.clone()])
                    .with_voices(voices)
                    .with_encodings(ENCODINGS.to_vec()),
            )
            .with_settings(settings_schema());
        Ok(Self {
            http: Http::new(config.http())?,
            model,
            descriptor,
            default_settings: config.default_settings.clone(),
        })
    }

    /// Replaces the advertised voice catalogue.
    ///
    /// A local server has its own voices, and nothing about this endpoint lets
    /// a client discover them.
    #[must_use]
    pub fn with_voices(mut self, voices: Vec<Voice>) -> Self {
        self.descriptor.metadata.voices = voices;
        self
    }

    /// The voices this synthesizer advertises.
    fn voices(&self) -> &[Voice] {
        &self.descriptor.metadata.voices
    }

    /// The voice to speak with, most specific choice first.
    ///
    /// A pipeline that names one gets it. Otherwise the provider definition's
    /// own first voice is what the operator configured, and speaking as the
    /// hosted default instead would ignore the only voice they asked for —
    /// which on a local `openedai-speech` server is not even a voice it has.
    fn voice_for(&self, requested: Option<String>) -> String {
        requested
            .or_else(|| self.voices().first().map(|voice| voice.id.clone()))
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
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
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
            settings: crate::layered_settings(
                &self.default_settings,
                request.settings.as_map(),
            ),
        };
        tracing::debug!(model = %body.model, voice = %body.voice, "synthesizing");

        let response = self.http.send(self.http.post("audio/speech").json(&body)).await?;

        // The endpoint sends raw audio with no framing, so a chunk is however
        // much has arrived. Forwarding them as they land is what lets playback
        // start before synthesis finishes.
        let name = self.http.name().to_owned();
        let format = request.format;

        // `unfold` over an `Option` rather than `map` over the body, because a
        // failed `reqwest` body re-reports the same error on *every* poll: a
        // plain `map` yields an unbounded stream of identical errors, so a
        // consumer draining until the stream ends never finishes and a lost
        // turn becomes a hung one. Taking the body out of the `Option` on the
        // first failure is what ends the stream after reporting it once.
        let chunks = futures_util::stream::unfold(
            (Some(response.bytes_stream()), 0_u64),
            move |(body, sequence)| {
                let name = name.clone();
                async move {
                    let mut body = body?;
                    match body.next().await {
                        Some(Ok(data)) => Some((
                            Ok(SpeechChunk { sequence, format, data }),
                            (Some(body), sequence + 1),
                        )),
                        // Audio that stops arriving partway through is
                        // classified like any other transport failure, so a
                        // caller can tell a stalled server from a rejected
                        // request. The body is dropped rather than polled again.
                        Some(Err(error)) => Some((
                            Err(Error::provider(&name, Failure::transport(&error))),
                            (None, sequence),
                        )),
                        None => None,
                    }
                }
            },
        );

        Ok(Box::pin(chunks))
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

    #[test]
    fn the_descriptor_carries_the_catalogue_and_the_encodings() {
        let provider = synthesizer().with_voices(vec![voice("shimmer")]);
        let metadata = &provider.descriptor().metadata;

        assert_eq!(metadata.voices, [voice("shimmer")], "the catalogue is read, not awaited");
        assert!(metadata.supports_encoding(Encoding::Opus));
        assert!(!metadata.supports_encoding(Encoding::PcmF32Le));
    }
}
