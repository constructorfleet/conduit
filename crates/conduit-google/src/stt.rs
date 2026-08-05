//! Speech-to-text over Cloud Speech-to-Text `speech:recognize`.
//!
//! # This endpoint takes a recording, not a stream
//!
//! `speech:recognize` is the batch method: it takes a complete utterance as
//! base64 in the request body and answers with the transcript. Google's
//! streaming recognizer is `StreamingRecognize`, which is gRPC-only — there is
//! no REST method for it — so this provider buffers the utterance and emits a
//! single final transcript. It reports no partials, because it genuinely has
//! none, and a provider that invented them would make the pipeline look more
//! responsive than it is.
//!
//! Google caps the inline `audio.content` at roughly one minute of audio;
//! anything longer needs `longrunningrecognize` and a Cloud Storage URI. An
//! utterance in a voice pipeline is seconds long, so the cap is not reached in
//! practice, and a recording that does exceed it earns Google's own
//! `INVALID_ARGUMENT` rather than a silent truncation here.
//!
//! # The sample rate has to be right
//!
//! `sampleRateHertz` tells Google how to interpret the bytes. Getting it wrong
//! does not fail — it produces a confidently wrong transcript of audio played at
//! the wrong speed. So it is taken from the format the caller declares its audio
//! to be in, never assumed.

use base64::Engine as _;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::auth::Tokens;
use crate::http::Http;
use crate::GoogleConfig;

/// The recognition path under the service's base URL.
const RECOGNIZE_PATH: &str = "speech:recognize";

/// The encodings this provider can describe to Google.
///
/// Google also accepts `MULAW`, `AMR`, `OGG_OPUS`, and more; only these three
/// have an [`Encoding`] to arrive as. Raw Opus frames are absent because Google
/// wants them in an Ogg container, and packaging one is not this crate's job.
const ENCODINGS: [Encoding; 2] = [Encoding::PcmS16Le, Encoding::Flac];

/// What Google accepts beyond a language and a format.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "enableAutomaticPunctuation": {
                "type": "boolean",
                "description": "Add punctuation and capitalization to the transcript.",
            },
            "profanityFilter": {
                "type": "boolean",
                "description": "Replace all but the first character of a profanity with asterisks.",
            },
            "maxAlternatives": {
                "type": "integer",
                "minimum": 1,
                "maximum": 30,
                "description": "How many transcripts to return. Only the best is used.",
            },
            "useEnhanced": {
                "type": "boolean",
                "description": "Use the enhanced model for the chosen model, where one exists.",
            },
        },
    }))
    .expect("a literal object schema")
}

/// A recognizer served by Cloud Speech-to-Text.
#[derive(Debug, Clone)]
pub struct GoogleStt {
    http: Http,
    descriptor: Descriptor,
    language: String,
    model: Option<String>,
    default_settings: serde_json::Map<String, serde_json::Value>,
}

impl GoogleStt {
    /// Builds a recognizer from `config`, resolving its credentials now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the configured language is not a well-formed
    /// BCP-47 tag, if credentials cannot be resolved — including in a build
    /// compiled without the `google` feature, whose message names it — or if the
    /// HTTP client cannot be built.
    pub async fn new(config: &GoogleConfig) -> Result<Self> {
        crate::validate_language(&config.language)?;

        let tokens = Tokens::resolve(&config.name, &config.credentials).await?;
        let http = Http::new(
            &config.name,
            &config.stt_base_url,
            tokens,
            config.connect_timeout,
            config.read_timeout,
        )?;

        let descriptor = config
            .descriptor(Capability::Stt)
            .with_metadata(
                Metadata::default()
                    .with_languages(vec![config.language.clone()])
                    .with_models(config.model.iter().cloned().collect())
                    .with_encodings(ENCODINGS.to_vec()),
            )
            .with_settings(settings_schema());

        Ok(Self {
            http,
            descriptor,
            language: config.language.clone(),
            model: config.model.clone(),
            default_settings: config.default_settings.clone(),
        })
    }

    /// The language to listen for: the session's hint, or this provider's own.
    ///
    /// `languageCode` is required, and Google's v1 recognizer has no "detect the
    /// language" mode — `alternativeLanguageCodes` narrows a list rather than
    /// opening one — so a session that names no language gets the configured tag
    /// rather than an omitted field that would 400.
    fn language_for(&self, requested: Option<String>) -> String {
        requested.unwrap_or_else(|| self.language.clone())
    }
}

/// A recognition request in Google's shape.
#[derive(Debug, Serialize)]
struct Request {
    config: RecognitionConfig,
    audio: RecognitionAudio,
}

/// How to interpret the audio, and what to do with it.
#[derive(Debug, Serialize)]
struct RecognitionConfig {
    encoding: &'static str,
    /// Samples per second. Wrong here means a wrong transcript, not an error.
    #[serde(rename = "sampleRateHertz")]
    sample_rate_hertz: u32,
    #[serde(rename = "audioChannelCount")]
    audio_channel_count: u16,
    #[serde(rename = "languageCode")]
    language_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(rename = "enableAutomaticPunctuation", skip_serializing_if = "Option::is_none")]
    enable_automatic_punctuation: Option<bool>,
    #[serde(rename = "profanityFilter", skip_serializing_if = "Option::is_none")]
    profanity_filter: Option<bool>,
    #[serde(rename = "maxAlternatives", skip_serializing_if = "Option::is_none")]
    max_alternatives: Option<u32>,
    #[serde(rename = "useEnhanced", skip_serializing_if = "Option::is_none")]
    use_enhanced: Option<bool>,
}

/// The recording, inline.
#[derive(Debug, Serialize)]
struct RecognitionAudio {
    content: String,
}

/// The service's answer.
#[derive(Debug, Default, Deserialize)]
struct Response {
    #[serde(default)]
    results: Vec<RecognitionResult>,
}

/// One recognized segment.
#[derive(Debug, Deserialize)]
struct RecognitionResult {
    #[serde(default)]
    alternatives: Vec<Alternative>,
    /// Where this segment ends, as a duration like `"5.300s"`.
    #[serde(rename = "resultEndTime", default)]
    result_end_time: Option<String>,
    #[serde(rename = "languageCode", default)]
    language_code: Option<String>,
}

/// One candidate transcript for a segment.
#[derive(Debug, Deserialize)]
struct Alternative {
    #[serde(default)]
    transcript: String,
    #[serde(default)]
    confidence: Option<f32>,
}

/// The `encoding` value for an encoding.
///
/// # Errors
///
/// Returns [`Error::Config`] for encodings Google cannot be told about.
fn recognition_encoding(encoding: Encoding) -> Result<&'static str> {
    match encoding {
        Encoding::PcmS16Le => Ok("LINEAR16"),
        Encoding::Flac => Ok("FLAC"),
        Encoding::PcmF32Le => Err(Error::Config(
            "Cloud Speech-to-Text does not accept 32-bit float PCM; send PcmS16Le".to_owned(),
        )),
        Encoding::Opus => Err(Error::Config(
            "Cloud Speech-to-Text needs Opus in an Ogg container, which this provider does not \
             build; send PcmS16Le or FLAC"
                .to_owned(),
        )),
        // `Encoding` is non-exhaustive; a format this code predates is one Google
        // was never told about.
        other => Err(Error::Config(format!(
            "Cloud Speech-to-Text was not told how to read {other:?}"
        ))),
    }
}

/// Seconds out of a protobuf duration string like `"5.300s"`, as milliseconds.
///
/// Returns `None` for anything that is not that shape, because a start offset is
/// a nicety and guessing one is worse than omitting it.
fn duration_ms(value: &str) -> Option<u64> {
    let seconds: f64 = value.strip_suffix('s')?.parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some((seconds * 1_000.0) as u64)
}

/// The transcript out of a recognition response.
///
/// Google splits a recording into segments, each with its own N-best list. The
/// pipeline wants one final transcript, so the best alternative of each segment
/// is joined in order — which is what the segments are, consecutive spans of the
/// same recording.
fn transcript_of(response: &Response) -> Transcript {
    let best: Vec<&Alternative> =
        response.results.iter().filter_map(|result| result.alternatives.first()).collect();

    let text = best
        .iter()
        .map(|alternative| alternative.transcript.trim())
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    // The confidence of the whole is the least confident part of it: a
    // transcript is only as trustworthy as its shakiest segment, and averaging
    // would hide one bad span inside a long clear utterance.
    let confidence = best
        .iter()
        .filter_map(|alternative| alternative.confidence)
        .fold(None::<f32>, |lowest, value| Some(lowest.map_or(value, |low| low.min(value))));

    // `resultEndTime` is where a segment *ends*, so the first segment's end is
    // not where speech began. The start of the recording is, and that is zero —
    // reported only when Google said something about timing at all.
    let start_ms = response
        .results
        .first()
        .and_then(|result| result.result_end_time.as_deref())
        .and_then(duration_ms)
        .map(|_| 0);

    Transcript {
        confidence,
        language: response.results.first().and_then(|result| result.language_code.clone()),
        start_ms,
        ..Transcript::final_text(text)
    }
}

#[async_trait::async_trait]
impl Provider for GoogleStt {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        // There is no cheap probe on this service — no listing, no status
        // endpoint — so the probe is a recognition of a moment of silence. It
        // exercises the credential and the endpoint, and transcribes to nothing.
        let silence = vec![0_u8; 320];
        let body = Request {
            config: RecognitionConfig {
                encoding: "LINEAR16",
                sample_rate_hertz: AudioFormat::DEFAULT.sample_rate,
                audio_channel_count: AudioFormat::DEFAULT.channels,
                language_code: self.language.clone(),
                model: self.model.clone(),
                enable_automatic_punctuation: None,
                profanity_filter: None,
                max_alternatives: None,
                use_enhanced: None,
            },
            audio: RecognitionAudio {
                content: base64::engine::general_purpose::STANDARD.encode(&silence),
            },
        };
        match self.http.post_json(RECOGNIZE_PATH, &body).await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl SpeechToText for GoogleStt {
    async fn transcribe(
        &self,
        mut audio: ChunkStream<AudioChunk>,
        options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>> {
        let encoding = recognition_encoding(options.format.encoding)?;
        let language = self.language_for(options.language);
        crate::validate_language(&language)?;
        if options.format.sample_rate == 0 || options.format.channels == 0 {
            return Err(Error::Config(format!(
                "audio declared {} Hz and {} channels, which cannot be recognized",
                options.format.sample_rate, options.format.channels
            )));
        }

        // The endpoint takes a recording, so the utterance is collected before
        // anything is sent. See the module documentation for why there is no
        // partial to emit in the meantime.
        let mut samples = Vec::new();
        while let Some(chunk) = audio.next().await {
            samples.extend_from_slice(&chunk?.data);
        }
        let captured = samples.len();

        let settings =
            crate::layered_settings(&self.default_settings, options.settings.as_map());
        let body = Request {
            config: RecognitionConfig {
                encoding,
                // Taken from what the caller declared, never assumed: audio read
                // at the wrong rate transcribes confidently and wrongly.
                sample_rate_hertz: options.format.sample_rate,
                audio_channel_count: options.format.channels,
                language_code: language,
                model: self.model.clone(),
                enable_automatic_punctuation: settings
                    .get("enableAutomaticPunctuation")
                    .and_then(serde_json::Value::as_bool),
                profanity_filter: settings
                    .get("profanityFilter")
                    .and_then(serde_json::Value::as_bool),
                max_alternatives: settings
                    .get("maxAlternatives")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                use_enhanced: settings.get("useEnhanced").and_then(serde_json::Value::as_bool),
            },
            audio: RecognitionAudio {
                content: base64::engine::general_purpose::STANDARD.encode(&samples),
            },
        };
        tracing::debug!(
            provider = %self.http.name(),
            language = %body.config.language_code,
            encoding,
            sample_rate = body.config.sample_rate_hertz,
            captured,
            "transcribing utterance"
        );

        let response = self.http.post_json(RECOGNIZE_PATH, &body).await?;
        // A body that is not the documented shape will not become one on a second
        // attempt. `body_failure` says so, while still reporting a body that
        // stalled halfway as the timeout it is.
        let recognized: Response = response
            .json()
            .await
            .map_err(|error| self.http.body_failure("transcript", error))?;

        let transcript = transcript_of(&recognized);
        tracing::debug!(
            provider = %self.http.name(),
            characters = transcript.text.len(),
            "transcription complete"
        );
        Ok(Box::pin(futures_util::stream::once(async move { Ok(transcript) })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> GoogleConfig {
        GoogleConfig {
            credentials: crate::Credentials::Token("t0ken".to_owned()),
            ..GoogleConfig::default()
        }
    }

    async fn recognizer(config: GoogleConfig) -> GoogleStt {
        GoogleStt::new(&config).await.expect("a recognizer")
    }

    #[test]
    fn encodings_map_onto_googles_names() {
        assert_eq!(recognition_encoding(Encoding::PcmS16Le).expect("supported"), "LINEAR16");
        assert_eq!(recognition_encoding(Encoding::Flac).expect("supported"), "FLAC");
    }

    #[test]
    fn unacceptable_encodings_are_refused_with_an_actionable_message() {
        for encoding in [Encoding::PcmF32Le, Encoding::Opus] {
            let error = recognition_encoding(encoding).expect_err("unsupported");
            assert!(error.to_string().contains("PcmS16Le"), "{error}");
        }
    }

    #[test]
    fn the_interchange_encoding_is_supported() {
        assert!(recognition_encoding(AudioFormat::DEFAULT.encoding).is_ok());
    }

    #[tokio::test]
    async fn a_session_language_overrides_the_configured_one() {
        let provider = recognizer(configured()).await;
        assert_eq!(provider.language_for(Some("de-DE".to_owned())), "de-DE");
        assert_eq!(provider.language_for(None), "en-US", "the configured tag is the fallback");
    }

    #[tokio::test]
    async fn a_bad_language_is_refused_at_construction() {
        let config = GoogleConfig { language: "en-US?pageSize=1".to_owned(), ..configured() };
        assert!(GoogleStt::new(&config).await.is_err());
    }

    #[tokio::test]
    async fn the_descriptor_names_the_encodings_and_the_model() {
        let provider =
            recognizer(GoogleConfig { model: Some("latest_long".to_owned()), ..configured() })
                .await;
        let metadata = &provider.descriptor().metadata;

        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(metadata.supports_encoding(Encoding::Flac));
        assert!(!metadata.supports_encoding(Encoding::Opus));
        assert_eq!(metadata.models, ["latest_long"]);
        assert_eq!(metadata.languages, ["en-US"]);
    }

    #[test]
    fn protobuf_durations_become_milliseconds() {
        assert_eq!(duration_ms("5.300s"), Some(5_300));
        assert_eq!(duration_ms("0s"), Some(0));
        assert_eq!(duration_ms("12s"), Some(12_000));
    }

    #[test]
    fn a_duration_that_is_not_one_is_omitted_rather_than_guessed() {
        for value in ["", "5.3", "later", "-1s", "s"] {
            assert_eq!(duration_ms(value), None, "{value} should not parse");
        }
    }

    fn response(json: serde_json::Value) -> Response {
        serde_json::from_value(json).expect("a response")
    }

    #[test]
    fn one_segment_becomes_one_final_transcript() {
        let recognized = response(serde_json::json!({
            "results": [{
                "alternatives": [{ "transcript": "turn on the kitchen light", "confidence": 0.97 }],
                "resultEndTime": "2.100s",
                "languageCode": "en-us",
            }],
        }));

        let transcript = transcript_of(&recognized);
        assert_eq!(transcript.text, "turn on the kitchen light");
        assert!(transcript.is_final, "this endpoint has only finals to give");
        assert_eq!(transcript.confidence, Some(0.97));
        assert_eq!(transcript.language.as_deref(), Some("en-us"));
        assert_eq!(transcript.start_ms, Some(0));
    }

    #[test]
    fn consecutive_segments_are_joined_in_order() {
        let recognized = response(serde_json::json!({
            "results": [
                { "alternatives": [{ "transcript": "turn on", "confidence": 0.9 }] },
                { "alternatives": [{ "transcript": " the light", "confidence": 0.8 }] },
            ],
        }));

        let transcript = transcript_of(&recognized);
        assert_eq!(transcript.text, "turn on the light");
    }

    #[test]
    fn only_the_best_alternative_of_each_segment_is_used() {
        // `maxAlternatives` asks for an N-best list; handing the pipeline all of
        // them concatenated would be gibberish.
        let recognized = response(serde_json::json!({
            "results": [{
                "alternatives": [
                    { "transcript": "turn on the light", "confidence": 0.95 },
                    { "transcript": "turn on the night", "confidence": 0.42 },
                ],
            }],
        }));

        assert_eq!(transcript_of(&recognized).text, "turn on the light");
    }

    #[test]
    fn the_whole_is_only_as_confident_as_its_shakiest_segment() {
        let recognized = response(serde_json::json!({
            "results": [
                { "alternatives": [{ "transcript": "clear as day", "confidence": 0.99 }] },
                { "alternatives": [{ "transcript": "mumble", "confidence": 0.31 }] },
            ],
        }));

        assert_eq!(transcript_of(&recognized).confidence, Some(0.31));
    }

    #[test]
    fn silence_recognizes_to_an_empty_final_rather_than_an_error() {
        // Google answers a recording with no speech in it with `{}` — no
        // `results` at all. That is a successful recognition of nothing, and the
        // pipeline needs the final to know the turn is over.
        let transcript = transcript_of(&response(serde_json::json!({})));
        assert_eq!(transcript.text, "");
        assert!(transcript.is_final);
        assert_eq!(transcript.confidence, None);
        assert_eq!(transcript.start_ms, None, "no timing was reported, so none is claimed");
    }

    #[test]
    fn a_segment_with_no_alternatives_is_skipped_rather_than_panicking() {
        let recognized = response(serde_json::json!({
            "results": [
                { "alternatives": [] },
                { "alternatives": [{ "transcript": "hello" }] },
            ],
        }));
        assert_eq!(transcript_of(&recognized).text, "hello");
    }
}
