//! Speech-to-text over the audio transcriptions API.
//!
//! Whisper servers — OpenAI's, Speaches, `whisper.cpp`'s, `faster-whisper` —
//! all expose this endpoint, so one implementation covers them.

use conduit_core::Result;
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::StreamExt;
use serde::Deserialize;

use crate::http::Http;
use crate::{wav, OpenAiConfig};

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
}

impl OpenAiStt {
    /// Builds a recognizer using `model`, e.g. `"whisper-1"`.
    ///
    /// # Errors
    ///
    /// Returns [`conduit_core::Error::Config`] if the HTTP client cannot be
    /// built.
    pub fn new(config: &OpenAiConfig, model: impl Into<String>) -> Result<Self> {
        Ok(Self { http: Http::new(config)?, model: model.into() })
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiStt {
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
        let upload = wav::package(options.format, samples)?;
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

        let response =
            self.http.send(self.http.post("audio/transcriptions").multipart(form)).await?;
        let body: Response = response.json().await.map_err(|error| self.http.failure(error))?;

        let transcript =
            Transcript { language: body.language, ..Transcript::final_text(body.text.trim()) };
        Ok(Box::pin(futures_util::stream::once(async move { Ok(transcript) })))
    }
}
