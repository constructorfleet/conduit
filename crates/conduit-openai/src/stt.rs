//! Speech-to-text over the audio transcriptions API.
//!
//! Whisper servers — OpenAI's, Speaches, `whisper.cpp`'s, `faster-whisper` —
//! all expose this endpoint, so one implementation covers them.

use conduit_core::audio::Encoding;
use conduit_core::Result;
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};
use futures_util::StreamExt;
use serde::Deserialize;

use crate::http::Http;
use crate::OpenAiConfig;

/// The API's response to a transcription request.
#[derive(Debug, Deserialize)]
struct Response {
    /// The recognized text.
    text: String,
    /// Detected language, when the server reports one.
    #[serde(default)]
    language: Option<String>,
}

/// A recognizer served over the audio transcriptions API.
///
/// The endpoint takes a complete recording rather than a stream, so this
/// provider buffers the utterance and emits a single final transcript. It
/// reports no partials, because it genuinely has none — a provider that
/// invented them would make the pipeline look more responsive than it is.
#[derive(Debug, Clone)]
pub struct OpenAiStt {
    http: Http,
    model: String,
    descriptor: Descriptor,
}

/// The encodings the transcriptions endpoint accepts, via the WAV container
/// Conduit packages an utterance into.
const ENCODINGS: [Encoding; 3] = [Encoding::PcmS16Le, Encoding::PcmF32Le, Encoding::Flac];

/// What the transcriptions endpoint accepts beyond a language hint.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "Vocabulary hint biasing the recognizer, e.g. proper nouns.",
            },
            "temperature": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0,
                "description": "Sampling temperature; 0 asks for the most likely transcript.",
            },
        },
    }))
    .expect("a literal object schema")
}

impl OpenAiStt {
    /// Builds a recognizer using `model`, e.g. `"whisper-1"`.
    ///
    /// # Errors
    ///
    /// Returns [`conduit_core::Error::Config`] if the HTTP client cannot be
    /// built.
    pub fn new(config: &OpenAiConfig, model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        let descriptor = config
            .descriptor(Capability::Stt)
            .with_metadata(
                Metadata::default()
                    .with_models(vec![model.clone()])
                    .with_encodings(ENCODINGS.to_vec()),
            )
            .with_settings(settings_schema());
        Ok(Self { http: Http::new(config)?, model, descriptor })
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiStt {
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
impl SpeechToText for OpenAiStt {
    async fn transcribe(
        &self,
        mut audio: ChunkStream<AudioChunk>,
        options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>> {
        let mut samples = Vec::new();
        while let Some(chunk) = audio.next().await {
            samples.extend_from_slice(&chunk?.data);
        }

        let captured = samples.len();
        let upload = conduit_core::wav::package(options.format, samples)?;
        tracing::debug!(model = %self.model, captured, "transcribing utterance");

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part(
                "file",
                reqwest::multipart::Part::bytes(upload.bytes)
                    .file_name(upload.filename)
                    .mime_str(upload.mime)
                    .map_err(|error| self.http.failure(error))?,
            );
        if let Some(language) = options.language {
            form = form.text("language", language);
        }
        // Already checked against this provider's declared schema, so each one
        // is a field the endpoint has rather than a blob forwarded on trust.
        for (name, value) in options.settings.as_map() {
            let rendered = match value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            form = form.text(name.clone(), rendered);
        }

        let response =
            self.http.send(self.http.post("audio/transcriptions").multipart(form)).await?;
        // A body that is not the documented shape will not become one on a
        // second attempt. `body_failure` says so, while still reporting a body
        // that stalled halfway as the timeout it is.
        let body: Response = response
            .json()
            .await
            .map_err(|error| self.http.body_failure("transcription", error))?;

        let transcript =
            Transcript { language: body.language, ..Transcript::final_text(body.text.trim()) };
        Ok(Box::pin(futures_util::stream::once(async move { Ok(transcript) })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_names_the_encodings_the_endpoint_takes() {
        let provider = OpenAiStt::new(&OpenAiConfig::default(), "whisper-1").expect("client");
        let metadata = &provider.descriptor().metadata;

        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(!metadata.supports_encoding(Encoding::Opus));
        assert_eq!(metadata.models, ["whisper-1"]);
    }
}
