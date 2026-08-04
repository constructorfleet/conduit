//! Text-to-speech over Cloud Text-to-Speech `text:synthesize`.
//!
//! # This endpoint does not stream
//!
//! `text:synthesize` is synchronous. It takes the whole input, processes it, and
//! answers with one base64 `audioContent` field holding the complete utterance.
//! There is no partial response and no chunked delivery to subscribe to; the
//! only streaming synthesis Google offers is `StreamingSynthesize`, which exists
//! on the gRPC surface and has no REST method in `v1` or in `v1beta1`.
//!
//! [`TextToSpeech`] streams, so this provider has to reconcile the two. It does
//! it the honest way: it awaits the whole response, then cuts the decoded buffer
//! into chunks and emits them. **Time to first audio is the full synthesis
//! latency of the entire utterance**, not the latency of its first syllable.
//! Chunking bounds how much a consumer holds and lets a dropped stream stop
//! delivery early; it does not make the first chunk arrive any sooner, and this
//! module does not pretend otherwise.
//!
//! # LINEAR16 arrives wrapped in a WAV header
//!
//! Google documents `audioContent` as including container headers, and says of
//! this one specifically: "For LINEAR16 audio, we include the WAV header."
//! [`SpeechChunk`] carries raw samples, so the header is parsed and removed —
//! and it is worth more than that, because it is Google's own statement of what
//! it produced. When it disagrees with what was asked for, the format on the
//! chunks is the one from the header, so a consumer resamples rather than
//! playing 24 kHz audio as though it were 16 kHz.

use base64::Engine as _;
use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};
use serde::{Deserialize, Serialize};

use crate::auth::Tokens;
use crate::http::Http;
use crate::GoogleConfig;

/// The synthesis path under the service's base URL.
const SYNTHESIZE_PATH: &str = "text:synthesize";

/// The voice catalogue path under the service's base URL.
const VOICES_PATH: &str = "voices";

/// The encodings this provider asks Google for and hands on.
///
/// Google also produces `MP3`, `MULAW`, and `ALAW`; none has an [`Encoding`], so
/// none is reachable through this interface.
const ENCODINGS: [Encoding; 2] = [Encoding::PcmS16Le, Encoding::Opus];

/// What Google accepts beyond a voice, a format, and a rate.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "pitch": {
                "type": "number",
                "minimum": -20.0,
                "maximum": 20.0,
                "description": "Semitones away from the voice's natural pitch; 0 is unchanged.",
            },
            "volumeGainDb": {
                "type": "number",
                "minimum": -96.0,
                "maximum": 16.0,
                "description": "Volume change in dB; 0 is the voice's natural volume.",
            },
            "ssmlGender": {
                "type": "string",
                "enum": ["MALE", "FEMALE", "NEUTRAL"],
                "description": "Preferred gender when no voice is named. Ignored when one is.",
            },
            "ssml": {
                "type": "boolean",
                "description": "Send the text as SSML rather than as plain text.",
            },
        },
    }))
    .expect("a literal object schema")
}

/// A synthesizer served by Cloud Text-to-Speech.
#[derive(Debug, Clone)]
pub struct GoogleTts {
    http: Http,
    descriptor: Descriptor,
    language: String,
    voice: Option<String>,
    chunk_bytes: usize,
    default_settings: serde_json::Map<String, serde_json::Value>,
}

impl GoogleTts {
    /// Builds a synthesizer from `config`, resolving its credentials now.
    ///
    /// Credentials are resolved at construction rather than at first request, so
    /// an operator saving a definition on a host with none is told while they are
    /// looking at the form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the configured language or voice is not a
    /// well-formed name, if credentials cannot be resolved — including in a build
    /// compiled without the `google` feature, whose message names it — or if the
    /// HTTP client cannot be built.
    pub async fn new(config: &GoogleConfig) -> Result<Self> {
        crate::validate_language(&config.language)?;
        if let Some(voice) = &config.voice {
            crate::validate_voice(voice)?;
        }

        let tokens = Tokens::resolve(&config.name, &config.credentials).await?;
        let http = Http::new(
            &config.name,
            &config.tts_base_url,
            tokens,
            config.connect_timeout,
            config.read_timeout,
        )?;

        // The configured voice is the catalogue until `refresh_voices` fetches
        // the real one: a descriptor built before any request can only advertise
        // what the operator named, and advertising nothing would leave an
        // operator screen unable to show the voice they chose.
        let voices = config
            .voice
            .iter()
            .map(|name| Voice {
                id: name.clone(),
                name: name.clone(),
                language: config.language.clone(),
            })
            .collect();
        let descriptor = config
            .descriptor(Capability::Tts)
            .with_metadata(
                Metadata::default()
                    .with_languages(vec![config.language.clone()])
                    .with_voices(voices)
                    .with_encodings(ENCODINGS.to_vec()),
            )
            .with_settings(settings_schema());

        Ok(Self {
            http,
            descriptor,
            language: config.language.clone(),
            voice: config.voice.clone(),
            chunk_bytes: config.chunk_bytes.max(1),
            default_settings: config.default_settings.clone(),
        })
    }

    /// Replaces the advertised voice catalogue with what `GET /v1/voices`
    /// reports.
    ///
    /// A separate call rather than part of construction: building a provider
    /// should not depend on a network round trip, and a catalogue that failed to
    /// load must not stop a correctly configured provider from speaking.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if the catalogue cannot be fetched or read.
    pub async fn refresh_voices(&mut self) -> Result<&[Voice]> {
        self.descriptor.metadata.voices = self.fetch_voices().await?;
        Ok(&self.descriptor.metadata.voices)
    }

    /// The voice catalogue for this provider's language.
    async fn fetch_voices(&self) -> Result<Vec<Voice>> {
        // Checked at construction, and checked again here because this is the
        // one value that reaches a URL query string.
        crate::validate_language(&self.language)?;
        let response =
            self.http.get(VOICES_PATH, &[("languageCode", self.language.as_str())]).await?;
        let listing: VoiceListing = response
            .json()
            .await
            .map_err(|error| self.http.body_failure("voice catalogue", error))?;
        Ok(listing.voices.into_iter().map(Voice::from).collect())
    }

    /// The voices this synthesizer advertises.
    #[must_use]
    pub fn voices(&self) -> &[Voice] {
        &self.descriptor.metadata.voices
    }

    /// The voice to speak with, most specific choice first.
    ///
    /// A pipeline that names one gets it. Otherwise the provider definition's
    /// configured voice is what the operator asked for. With neither, `None`
    /// lets Google pick a voice for the language — the request is still valid,
    /// because `languageCode` is the only required part of `voice`.
    fn voice_for(&self, requested: Option<String>) -> Option<String> {
        requested.or_else(|| self.voice.clone())
    }

    /// The language to speak, taken from the voice when the voice names one.
    ///
    /// A Google voice name begins with its own language — `de-DE-Neural2-B` is a
    /// German voice — and Google rejects a request whose `languageCode` and
    /// `name` disagree. So a pipeline naming a voice is naming a language too,
    /// and honouring the voice means deriving the tag from it rather than sending
    /// the provider's configured one beside it.
    fn language_for(&self, voice: Option<&str>) -> String {
        voice.and_then(language_of_voice).unwrap_or_else(|| self.language.clone())
    }
}

/// The language prefix of a Google voice name, e.g. `en-US` of
/// `en-US-Neural2-F`.
///
/// Returns `None` for a name that does not begin with two subtags, which is a
/// name this function should not second-guess.
fn language_of_voice(voice: &str) -> Option<String> {
    let mut parts = voice.split('-');
    let language = parts.next()?;
    let region = parts.next()?;
    // A voice's region subtag is a region — `US`, `GB`, `419` — and a name whose
    // second part is a family rather than a region ("en-Neural2") would produce
    // a language tag Google does not have.
    let region_shaped = (2..=3).contains(&region.len())
        && (region.chars().all(|c| c.is_ascii_uppercase())
            || region.chars().all(|c| c.is_ascii_digit()));
    if language.len() < 2 || !region_shaped {
        return None;
    }
    Some(format!("{language}-{region}"))
}

/// A synthesis request in Google's shape.
#[derive(Debug, Serialize)]
struct Request {
    input: Input,
    voice: VoiceSelection,
    #[serde(rename = "audioConfig")]
    audio_config: AudioConfig,
}

/// What to say, as text or as SSML. Google rejects a request carrying both.
#[derive(Debug, Serialize)]
struct Input {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssml: Option<String>,
}

/// Who says it. `languageCode` is required; everything else is a preference.
#[derive(Debug, Serialize)]
struct VoiceSelection {
    #[serde(rename = "languageCode")]
    language_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(rename = "ssmlGender", skip_serializing_if = "Option::is_none")]
    ssml_gender: Option<String>,
}

/// How it comes back.
#[derive(Debug, Serialize)]
struct AudioConfig {
    #[serde(rename = "audioEncoding")]
    audio_encoding: &'static str,
    #[serde(rename = "sampleRateHertz")]
    sample_rate_hertz: u32,
    #[serde(rename = "speakingRate", skip_serializing_if = "Option::is_none")]
    speaking_rate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pitch: Option<f64>,
    #[serde(rename = "volumeGainDb", skip_serializing_if = "Option::is_none")]
    volume_gain_db: Option<f64>,
}

/// The service's answer: the whole utterance, base64-encoded.
#[derive(Debug, Deserialize)]
struct Response {
    #[serde(rename = "audioContent")]
    audio_content: String,
}

/// `GET /v1/voices`.
#[derive(Debug, Default, Deserialize)]
struct VoiceListing {
    #[serde(default)]
    voices: Vec<CatalogueVoice>,
}

/// One entry of the voice catalogue.
#[derive(Debug, Deserialize)]
struct CatalogueVoice {
    name: String,
    #[serde(rename = "languageCodes", default)]
    language_codes: Vec<String>,
}

impl From<CatalogueVoice> for Voice {
    fn from(voice: CatalogueVoice) -> Self {
        Self {
            language: voice
                .language_codes
                .first()
                .cloned()
                .or_else(|| language_of_voice(&voice.name))
                .unwrap_or_else(|| crate::DEFAULT_LANGUAGE.to_owned()),
            name: voice.name.clone(),
            id: voice.name,
        }
    }
}

/// The `audioEncoding` value for an encoding.
///
/// # Errors
///
/// Returns [`Error::Config`] for encodings Google does not produce.
fn audio_encoding(encoding: Encoding) -> Result<&'static str> {
    match encoding {
        Encoding::PcmS16Le => Ok("LINEAR16"),
        Encoding::Opus => Ok("OGG_OPUS"),
        Encoding::PcmF32Le => Err(Error::Config(
            "Cloud Text-to-Speech does not produce 32-bit float PCM; ask for PcmS16Le"
                .to_owned(),
        )),
        Encoding::Flac => Err(Error::Config(
            "Cloud Text-to-Speech does not produce FLAC; ask for PcmS16Le or Opus".to_owned(),
        )),
        // `Encoding` is non-exhaustive; a format this code predates is one Google
        // was never asked for.
        other => Err(Error::Config(format!("Cloud Text-to-Speech does not produce {other:?}"))),
    }
}

/// The samples out of `audioContent`, and what they actually are.
///
/// LINEAR16 arrives as a WAV file, so the header is removed and believed: it is
/// Google's own statement of the rate and channel count it produced, and it wins
/// over what was requested. Every other encoding is passed through with the
/// requested format, because there is no header to read it out of.
///
/// # Errors
///
/// Returns [`Error::Provider`] if the payload is not the shape its encoding
/// promises.
fn decode_audio(
    http: &Http,
    requested: AudioFormat,
    payload: Vec<u8>,
) -> Result<(AudioFormat, Vec<u8>)> {
    if requested.encoding != Encoding::PcmS16Le {
        return Ok((requested, payload));
    }
    let pcm = conduit_core::wav::parse(&payload).map_err(|error| {
        http.malformed(format!(
            "LINEAR16 audio was not the WAV file Google documents it to be: {error}"
        ))
    })?;
    if pcm.format != requested {
        // Not a failure — Google resamples to whatever rate is asked for, and a
        // voice that cannot reach it answers at its own. Reporting the rate that
        // arrived is what stops a consumer pitching it.
        tracing::debug!(
            provider = %http.name(),
            requested_rate = requested.sample_rate,
            produced_rate = pcm.format.sample_rate,
            produced_channels = pcm.format.channels,
            "Google produced a different format than requested"
        );
    }
    Ok((pcm.format, pcm.samples))
}

/// Cuts `samples` into chunks of at most `size` bytes, all in `format`.
///
/// Frame-aligned for PCM, because a chunk boundary inside a sample would split
/// one 16-bit value across two chunks and a consumer decoding each on its own
/// would hear a click.
fn chunk(format: AudioFormat, samples: Vec<u8>, size: usize) -> Vec<SpeechChunk> {
    let frame = match format.encoding {
        Encoding::PcmS16Le => 2 * usize::from(format.channels.max(1)),
        Encoding::PcmF32Le => 4 * usize::from(format.channels.max(1)),
        // Compressed payloads have no frame this code can see, so they are cut
        // wherever asked and reassembled by the decoder downstream.
        _ => 1,
    };
    let size = (size / frame.max(1)).max(1) * frame.max(1);

    let data = Bytes::from(samples);
    (0..data.len())
        .step_by(size)
        .enumerate()
        .map(|(index, start)| SpeechChunk {
            sequence: index as u64,
            format,
            data: data.slice(start..(start + size).min(data.len())),
        })
        .collect()
}

#[async_trait::async_trait]
impl Provider for GoogleTts {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        // The voice catalogue is the cheapest authenticated call this service
        // has: it exercises the credential and the network without synthesizing
        // anything or being billed for it.
        match self.fetch_voices().await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl TextToSpeech for GoogleTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        let audio_encoding = audio_encoding(request.format.encoding)?;
        let voice = self.voice_for(request.voice);
        if let Some(voice) = &voice {
            crate::validate_voice(voice)?;
        }
        let language = self.language_for(voice.as_deref());
        crate::validate_language(&language)?;

        let settings =
            crate::layered_settings(&self.default_settings, request.settings.as_map());
        let as_ssml =
            settings.get("ssml").and_then(serde_json::Value::as_bool).unwrap_or(false);

        let body = Request {
            input: if as_ssml {
                Input { text: None, ssml: Some(request.text) }
            } else {
                Input { text: Some(request.text), ssml: None }
            },
            voice: VoiceSelection {
                language_code: language,
                // `ssmlGender` is a preference for when no voice is named;
                // sending it beside a name is at best ignored.
                ssml_gender: voice
                    .is_none()
                    .then(|| {
                        settings
                            .get("ssmlGender")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten(),
                name: voice,
            },
            audio_config: AudioConfig {
                audio_encoding,
                sample_rate_hertz: request.format.sample_rate,
                speaking_rate: request.rate,
                pitch: settings.get("pitch").and_then(serde_json::Value::as_f64),
                volume_gain_db: settings
                    .get("volumeGainDb")
                    .and_then(serde_json::Value::as_f64),
            },
        };
        tracing::debug!(
            provider = %self.http.name(),
            language = %body.voice.language_code,
            voice = body.voice.name.as_deref().unwrap_or("<any>"),
            encoding = audio_encoding,
            "synthesizing"
        );

        // Awaited whole, because that is the only thing this endpoint offers.
        // See the module documentation: chunking below is delivery, not
        // streaming, and the first chunk waits for the last syllable.
        let response = self.http.post_json(SYNTHESIZE_PATH, &body).await?;
        let synthesized: Response = response
            .json()
            .await
            .map_err(|error| self.http.body_failure("synthesized audio", error))?;

        let payload = base64::engine::general_purpose::STANDARD
            .decode(synthesized.audio_content.as_bytes())
            .map_err(|error| {
                self.http.malformed(format!("audioContent was not valid base64: {error}"))
            })?;
        let (format, samples) = decode_audio(&self.http, request.format, payload)?;

        let chunks = chunk(format, samples, self.chunk_bytes);
        tracing::debug!(
            provider = %self.http.name(),
            chunks = chunks.len(),
            sample_rate = format.sample_rate,
            "synthesis complete"
        );
        Ok(Box::pin(futures_util::stream::iter(chunks.into_iter().map(Ok))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn synthesizer(config: GoogleConfig) -> GoogleTts {
        GoogleTts::new(&config).await.expect("a synthesizer")
    }

    fn configured() -> GoogleConfig {
        GoogleConfig {
            credentials: crate::Credentials::Token("t0ken".to_owned()),
            ..GoogleConfig::default()
        }
    }

    #[test]
    fn encodings_map_onto_googles_names() {
        assert_eq!(audio_encoding(Encoding::PcmS16Le).expect("supported"), "LINEAR16");
        assert_eq!(audio_encoding(Encoding::Opus).expect("supported"), "OGG_OPUS");
    }

    #[test]
    fn unproducible_encodings_are_refused_with_an_actionable_message() {
        for encoding in [Encoding::PcmF32Le, Encoding::Flac] {
            let error = audio_encoding(encoding).expect_err("unsupported");
            assert!(error.to_string().contains("PcmS16Le"), "{error}");
        }
    }

    #[test]
    fn the_interchange_encoding_is_supported() {
        assert!(audio_encoding(AudioFormat::DEFAULT.encoding).is_ok());
    }

    #[test]
    fn a_voice_name_yields_the_language_it_speaks() {
        assert_eq!(language_of_voice("en-US-Neural2-F").as_deref(), Some("en-US"));
        assert_eq!(language_of_voice("de-DE-Wavenet-A").as_deref(), Some("de-DE"));
        assert_eq!(language_of_voice("es-419-Neural2-A").as_deref(), Some("es-419"));
    }

    #[test]
    fn a_name_without_a_region_yields_no_language() {
        // Better to fall back to the configured tag than to invent `en-Neural2`.
        assert_eq!(language_of_voice("en-Neural2-F"), None);
        assert_eq!(language_of_voice("nonsense"), None);
        assert_eq!(language_of_voice("e-US"), None);
    }

    #[tokio::test]
    async fn a_requested_voice_wins_over_the_configured_one() {
        let provider = synthesizer(GoogleConfig {
            voice: Some("en-US-Neural2-A".to_owned()),
            ..configured()
        })
        .await;
        assert_eq!(
            provider.voice_for(Some("en-GB-Neural2-B".to_owned())).as_deref(),
            Some("en-GB-Neural2-B")
        );
    }

    #[tokio::test]
    async fn the_configured_voice_is_used_when_the_pipeline_names_none() {
        let provider = synthesizer(GoogleConfig {
            voice: Some("en-US-Neural2-A".to_owned()),
            ..configured()
        })
        .await;
        assert_eq!(provider.voice_for(None).as_deref(), Some("en-US-Neural2-A"));
    }

    #[tokio::test]
    async fn naming_no_voice_at_all_leaves_the_choice_to_google() {
        // `languageCode` is the only required part of `voice`, so this is still
        // a valid request rather than a reason to invent a voice name.
        assert_eq!(synthesizer(configured()).await.voice_for(None), None);
    }

    #[tokio::test]
    async fn a_voice_carries_its_own_language_over_the_configured_one() {
        // Google rejects a request whose `languageCode` and `name` disagree, so
        // honouring a German voice means sending `de-DE` beside it.
        let provider = synthesizer(GoogleConfig {
            language: "en-US".to_owned(),
            voice: Some("de-DE-Neural2-B".to_owned()),
            ..configured()
        })
        .await;
        assert_eq!(provider.language_for(Some("de-DE-Neural2-B")), "de-DE");
        assert_eq!(provider.language_for(None), "en-US");
    }

    #[tokio::test]
    async fn a_bad_language_or_voice_is_refused_at_construction() {
        let bad_language =
            GoogleConfig { language: "en-US&key=leaked".to_owned(), ..configured() };
        assert!(GoogleTts::new(&bad_language).await.is_err());

        let bad_voice =
            GoogleConfig { voice: Some("../etc/passwd".to_owned()), ..configured() };
        assert!(GoogleTts::new(&bad_voice).await.is_err());
    }

    #[tokio::test]
    async fn the_descriptor_advertises_the_encodings_and_the_configured_voice() {
        let provider = synthesizer(GoogleConfig {
            voice: Some("en-US-Neural2-F".to_owned()),
            ..configured()
        })
        .await;
        let metadata = &provider.descriptor().metadata;

        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(metadata.supports_encoding(Encoding::Opus));
        assert!(!metadata.supports_encoding(Encoding::Flac));
        assert_eq!(metadata.voices.len(), 1, "the operator's own voice is advertised");
        assert_eq!(metadata.voices[0].id, "en-US-Neural2-F");
        assert_eq!(metadata.languages, ["en-US"]);
    }

    #[test]
    fn a_catalogue_entry_becomes_a_voice() {
        let voice = Voice::from(CatalogueVoice {
            name: "en-GB-Neural2-C".to_owned(),
            language_codes: vec!["en-GB".to_owned()],
        });
        assert_eq!(voice.id, "en-GB-Neural2-C");
        assert_eq!(voice.name, "en-GB-Neural2-C");
        assert_eq!(voice.language, "en-GB");
    }

    #[test]
    fn a_catalogue_entry_without_a_language_falls_back_to_its_name() {
        let voice = Voice::from(CatalogueVoice {
            name: "ja-JP-Standard-A".to_owned(),
            language_codes: Vec::new(),
        });
        assert_eq!(voice.language, "ja-JP");
    }

    fn http() -> Http {
        Http::new(
            "google",
            "https://texttospeech.googleapis.com/v1",
            crate::auth::Tokens::Fixed(std::sync::Arc::from("t0ken")),
            std::time::Duration::from_secs(1),
            None,
        )
        .expect("a client")
    }

    #[test]
    fn a_wav_header_is_removed_and_its_format_believed() {
        // Google documents LINEAR16 as arriving with a WAV header. Handing 44
        // bytes of `RIFF....WAVEfmt ` on as though they were samples would play
        // as a click and embed as noise.
        let produced =
            AudioFormat { encoding: Encoding::PcmS16Le, sample_rate: 24_000, channels: 1 };
        let samples: Vec<u8> = (0..64).map(|byte| byte as u8).collect();
        let wrapped = conduit_core::wav::package(produced, samples.clone()).expect("wav").bytes;

        let (format, decoded) =
            decode_audio(&http(), AudioFormat::DEFAULT, wrapped).expect("readable");

        assert_eq!(decoded, samples, "the header is stripped, not passed through");
        assert_eq!(
            format.sample_rate, 24_000,
            "the rate Google produced is reported, not the one requested"
        );
    }

    #[test]
    fn linear16_that_is_not_a_wav_file_is_a_malformed_response() {
        let error = decode_audio(&http(), AudioFormat::DEFAULT, b"not a wav at all".to_vec())
            .expect_err("malformed");
        assert_eq!(
            crate::Failure::of(&error).map(crate::Failure::kind),
            Some(crate::FailureKind::Malformed)
        );
        assert!(!crate::Failure::of(&error).expect("classified").is_retryable());
    }

    #[test]
    fn a_compressed_payload_passes_through_with_the_requested_format() {
        // There is no header this code can read Opus's real rate out of, so the
        // requested format is the only claim available.
        let requested =
            AudioFormat { encoding: Encoding::Opus, sample_rate: 48_000, channels: 1 };
        let payload = b"OggS...".to_vec();
        let (format, decoded) =
            decode_audio(&http(), requested, payload.clone()).expect("passed through");
        assert_eq!(format, requested);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn chunks_are_numbered_from_zero_and_reassemble_to_the_whole() {
        let samples: Vec<u8> = (0..100).map(|byte| byte as u8).collect();
        let chunks = chunk(AudioFormat::DEFAULT, samples.clone(), 32);

        assert_eq!(chunks.len(), 4, "100 bytes in 32-byte pieces");
        assert_eq!(chunks.iter().map(|chunk| chunk.sequence).collect::<Vec<_>>(), [0, 1, 2, 3]);
        let rejoined: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.to_vec()).collect();
        assert_eq!(rejoined, samples, "nothing is lost or duplicated");
        assert!(chunks.iter().all(|chunk| chunk.format == AudioFormat::DEFAULT));
    }

    #[test]
    fn chunk_boundaries_never_split_a_sample() {
        // A 16-bit sample cut in half is a click on one side and a click on the
        // other.
        let samples: Vec<u8> = (0..64).map(|byte| byte as u8).collect();
        for size in [1, 3, 5, 7, 33] {
            let chunks = chunk(AudioFormat::DEFAULT, samples.clone(), size);
            for chunk in chunks.iter().take(chunks.len().saturating_sub(1)) {
                assert_eq!(chunk.data.len() % 2, 0, "size {size} split a sample");
            }
        }
    }

    #[test]
    fn a_stereo_frame_is_kept_whole_too() {
        let format = AudioFormat { channels: 2, ..AudioFormat::DEFAULT };
        let samples: Vec<u8> = (0..64).map(|byte| byte as u8).collect();
        let chunks = chunk(format, samples, 5);
        for chunk in chunks.iter().take(chunks.len() - 1) {
            assert_eq!(chunk.data.len() % 4, 0, "a stereo frame is four bytes");
        }
    }

    #[test]
    fn silence_produces_no_chunks_rather_than_an_empty_one() {
        assert!(chunk(AudioFormat::DEFAULT, Vec::new(), 32).is_empty());
    }
}
