//! Batch transcription over `POST /v1/speech-to-text`.
//!
//! The endpoint takes a complete recording as a multipart upload rather than a
//! stream, so this provider buffers the utterance and emits a single final
//! transcript. It reports no partials, because it genuinely has none — a
//! provider that invented them would make the pipeline look more responsive
//! than it is. Partials need the realtime websocket protocol, which this crate
//! does not implement; see the crate README.

use conduit_core::audio::Encoding;
use conduit_core::Result;
use conduit_http::Http;
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};
use futures_util::StreamExt;

use crate::{ElevenLabsConfig, DEFAULT_STT_MODEL, DEFAULT_STT_MODELS};

/// The encodings this provider accepts, via the WAV container Conduit packages
/// an utterance into.
///
/// The endpoint sniffs the uploaded file, and accepts WAV and FLAC among many
/// others. Raw Opus frames are absent because they are not a file:
/// [`conduit_core::wav::package`] refuses them, and it is right to — Opus needs
/// an Ogg container nothing here builds.
const ENCODINGS: [Encoding; 3] = [Encoding::PcmS16Le, Encoding::PcmF32Le, Encoding::Flac];

/// What the transcription endpoint accepts beyond a language hint.
///
/// A deliberately small subset of what the endpoint offers. The absences are
/// the point:
///
/// - `webhook`, `webhook_id`, and `source_url` change the *shape of the
///   response*: they make it an acknowledgement rather than a transcript. A
///   provider that returned one would report an empty utterance as a success.
/// - `use_multi_channel` returns `transcripts[]` instead of `text`, for the same
///   reason.
/// - `additional_formats` asks for SRT and the like, which a spoken turn has
///   nowhere to put.
/// - `entity_detection`, `entity_redaction`, `keyterms`, and
///   `detect_speaker_roles` each carry a documented surcharge, so they are not
///   things to enable by mistyping a schema.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "diarize": {
                "type": "boolean",
                "description":
                    "Annotate which speaker said what. Conduit identifies speakers with its \
                     own speaker capability, so this is off unless a deployment wants the \
                     vendor's labels in the transcript text.",
            },
            "num_speakers": {
                "type": "integer",
                "minimum": 1,
                "maximum": 32,
                "description": "How many speakers to expect, when diarizing.",
            },
            "tag_audio_events": {
                "type": "boolean",
                "description":
                    "Annotate non-speech events such as laughter in the transcript. Usually \
                     off for a voice assistant: `(laughs)` is not a command.",
            },
            "temperature": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 2.0,
                "description": "Sampling temperature; 0 asks for the most likely transcript.",
            },
            "seed": {
                "type": "integer",
                "minimum": 0,
                "maximum": 2147483647,
                "description": "Fixes sampling, for a reproducible transcript.",
            },
            "no_verbatim": {
                "type": "boolean",
                "description":
                    "Drop filler words and stutters. Reads better; loses what was actually \
                     said.",
            },
        },
    }))
    .expect("a literal object schema")
}

/// A recognizer served over the batch transcription endpoint.
#[derive(Debug, Clone)]
pub struct ElevenLabsStt {
    http: Http,
    model: String,
    descriptor: Descriptor,
    default_settings: serde_json::Map<String, serde_json::Value>,
}

impl ElevenLabsStt {
    /// Builds a recognizer from `config`.
    ///
    /// The model is the first entry of [`ElevenLabsConfig::models`], or
    /// [`DEFAULT_STT_MODEL`] when none are named.
    ///
    /// # Errors
    ///
    /// Returns [`conduit_core::Error::Config`] if the HTTP client cannot be
    /// built.
    pub fn new(config: &ElevenLabsConfig) -> Result<Self> {
        let models = config.models_or(DEFAULT_STT_MODELS);
        let model = models.first().cloned().unwrap_or_else(|| DEFAULT_STT_MODEL.to_owned());
        let descriptor = config
            .descriptor(Capability::Stt)
            .with_metadata(
                Metadata::default().with_models(models).with_encodings(ENCODINGS.to_vec()),
            )
            .with_settings(settings_schema());

        Ok(Self {
            http: Http::new(config.http())?,
            model,
            descriptor,
            default_settings: config.default_settings.clone(),
        })
    }
}

#[async_trait::async_trait]
impl Provider for ElevenLabsStt {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        // The voice catalogue, which is the cheapest authenticated GET the API
        // offers: `/v1/models` 404s, and transcribing a sample to check liveness
        // would bill an operator for a health check.
        match self.http.send(self.http.get("voices")).await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl SpeechToText for ElevenLabsStt {
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
        // Refused before the upload rather than after: raw samples with no
        // container are not a file the endpoint can sniff.
        let upload = conduit_core::wav::package(options.format, samples)?;
        tracing::debug!(
            provider = %self.http.name(),
            model = %self.model,
            captured,
            "transcribing utterance"
        );

        let mut form =
            reqwest::multipart::Form::new().text("model_id", self.model.clone()).part(
                "file",
                reqwest::multipart::Part::bytes(upload.bytes)
                    .file_name(upload.filename)
                    .mime_str(upload.mime)
                    .map_err(|error| self.http.failure(error))?,
            );
        if let Some(language) = options.language {
            // `language_code`, not `language`: the same hint under a different
            // name from the OpenAI endpoint, and a wrong name here is silently
            // ignored rather than rejected.
            form = form.text("language_code", language);
        }
        // The provider's stored defaults, overridden by anything the request
        // carries. Already checked against this provider's declared schema, so
        // each one is a field the endpoint has rather than a blob forwarded on
        // trust.
        for (name, value) in
            crate::layered_settings(&self.default_settings, options.settings.as_map())
        {
            let rendered = match value {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            };
            form = form.text(name, rendered);
        }

        let response = self.http.send(self.http.post("speech-to-text").multipart(form)).await?;
        // A body that is not the documented shape will not become one on a
        // second attempt. `body_failure` says so, while still reporting a body
        // that stalled halfway as the timeout it is.
        let body: crate::wire::Transcription = response
            .json()
            .await
            .map_err(|error| self.http.body_failure("transcription", error))?;

        // `language_probability` is confidence in the detected *language*, not
        // in the transcript, so it deliberately does not become
        // `Transcript::confidence` — a caller thresholding on confidence would
        // otherwise be thresholding on the wrong number.
        let transcript = Transcript {
            language: body.language_code,
            ..Transcript::final_text(body.text.trim())
        };
        Ok(Box::pin(futures_util::stream::once(async move { Ok(transcript) })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recognizer() -> ElevenLabsStt {
        ElevenLabsStt::new(&ElevenLabsConfig::default()).expect("client")
    }

    #[test]
    fn the_descriptor_names_the_models_and_the_encodings_the_endpoint_takes() {
        let provider = recognizer();
        let metadata = &provider.descriptor().metadata;

        assert_eq!(metadata.models, DEFAULT_STT_MODELS);
        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(metadata.supports_encoding(Encoding::Flac));
        assert!(!metadata.supports_encoding(Encoding::Opus), "raw frames are not a file");
    }

    #[test]
    fn the_first_named_model_is_the_one_used() {
        let provider = ElevenLabsStt::new(&ElevenLabsConfig {
            models: vec!["scribe_v1".to_owned()],
            ..Default::default()
        })
        .expect("client");

        assert_eq!(provider.model, "scribe_v1");
        assert_eq!(recognizer().model, DEFAULT_STT_MODEL, "and the default otherwise");
    }

    #[test]
    fn settings_that_would_change_the_response_shape_are_not_declared() {
        // A webhook or a multi-channel upload answers with an acknowledgement or
        // a `transcripts[]` array instead of a transcript. Accepting either would
        // report an empty utterance as a success.
        let descriptor = recognizer().descriptor().clone();
        for setting in [
            "webhook",
            "webhook_id",
            "source_url",
            "cloud_storage_url",
            "use_multi_channel",
            "additional_formats",
            "entity_detection",
            "entity_redaction",
            "keyterms",
            "detect_speaker_roles",
        ] {
            let value = serde_json::json!({ setting: true });
            assert!(
                descriptor.validate_settings(&value).is_err(),
                "`{setting}` must not be settable"
            );
        }
    }

    #[test]
    fn the_declared_settings_are_checked_against_their_documented_bounds() {
        let descriptor = recognizer().descriptor().clone();
        for value in [
            serde_json::json!({ "temperature": 2.5 }),
            serde_json::json!({ "num_speakers": 33 }),
            serde_json::json!({ "num_speakers": 0 }),
            serde_json::json!({ "diarize": "yes" }),
            serde_json::json!({ "seed": -1 }),
        ] {
            assert!(descriptor.validate_settings(&value).is_err(), "{value} should be refused");
        }
        assert!(descriptor
            .validate_settings(&serde_json::json!({ "diarize": true, "num_speakers": 2 }))
            .is_ok());
    }
}
