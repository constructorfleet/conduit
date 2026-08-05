//! Speech synthesis over `/v1/text-to-speech/{voice_id}/stream`.
//!
//! The streaming endpoint rather than the buffered one, because a spoken turn
//! should begin before synthesis finishes: audio arrives as raw PCM with no
//! framing, and every packet that lands is forwarded as a chunk.

use conduit_core::audio::Encoding;
use conduit_core::{Error, Result};
use conduit_http::{Failure, Http};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};
use futures_util::StreamExt;

use crate::wire::{OutputFormat, Synthesis, VoiceSettings};
use crate::{ElevenLabsConfig, DEFAULT_TTS_MODEL, DEFAULT_TTS_MODELS};

/// The encodings this provider produces.
///
/// Signed 16-bit PCM only. The endpoint also offers MP3, μ-law, A-law, and
/// Opus, and [`Encoding`] can name none of them except Opus — whose framing the
/// vendor does not document. See [`OutputFormat::for_request`].
const ENCODINGS: [Encoding; 1] = [Encoding::PcmS16Le];

/// What the synthesis endpoint accepts beyond a voice, a format, and a rate.
///
/// The voice controls, declared individually rather than as one open object, so
/// an operator who writes `similarity-boost` is told at save time rather than
/// having it silently dropped by the vendor. Bounds are the vendor's documented
/// ones, so a value out of range is refused here instead of becoming a 422.
///
/// `speed` is conspicuously absent: it is [`SynthesisRequest::rate`], read from
/// the request. Declaring it as well would send the same control twice with the
/// API choosing a winner.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "stability": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0,
                "description":
                    "How consistent the delivery is. Lower is more expressive and more \
                     variable between renders; higher is flatter and more predictable. \
                     Unset uses whatever is tuned on the voice itself.",
            },
            "similarity_boost": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0,
                "description":
                    "How closely the output tracks the original recording of the voice. \
                     High values can reproduce artefacts present in the source audio.",
            },
            "style": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0,
                "description":
                    "Style exaggeration. Costs latency, so 0 is the right answer for a \
                     spoken turn unless the voice needs it.",
            },
            "use_speaker_boost": {
                "type": "boolean",
                "description":
                    "Boosts similarity to the speaker at some latency cost.",
            },
            "language_code": {
                "type": "string",
                "description":
                    "ISO 639-1 code forcing the spoken language, where the model supports \
                     it. Unset lets the model infer it from the text.",
            },
        },
    }))
    .expect("a literal object schema")
}

/// A synthesizer served over the ElevenLabs streaming endpoint.
#[derive(Debug, Clone)]
pub struct ElevenLabsTts {
    http: Http,
    model: String,
    /// The voice used when a request names none, already validated.
    default_voice: Option<String>,
    descriptor: Descriptor,
    default_settings: serde_json::Map<String, serde_json::Value>,
}

impl ElevenLabsTts {
    /// Builds a synthesizer from `config`.
    ///
    /// The model is the first entry of [`ElevenLabsConfig::models`], or
    /// [`DEFAULT_TTS_MODEL`] when none are named.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built, or if
    /// [`ElevenLabsConfig::voice_id`] is not a value this crate will place in a
    /// URL path — which fails here, when the definition is saved, rather than on
    /// the first turn.
    pub fn new(config: &ElevenLabsConfig) -> Result<Self> {
        let models = config.models_or(DEFAULT_TTS_MODELS);
        let model = models.first().cloned().unwrap_or_else(|| DEFAULT_TTS_MODEL.to_owned());
        // Validated at construction, not at synthesis. A stored definition
        // carrying a traversal attempt should refuse to become a provider, so
        // the operator sees it while they are looking at the form.
        let default_voice = config
            .voice_id
            .as_deref()
            .map(|voice| crate::voice_id::validate(voice).map(str::to_owned))
            .transpose()?;
        let descriptor = config
            .descriptor(Capability::Tts)
            .with_metadata(
                Metadata::default()
                    .with_models(models)
                    .with_voices(config.voices.clone())
                    .with_encodings(ENCODINGS.to_vec()),
            )
            .with_settings(settings_schema());

        Ok(Self {
            http: Http::new(config.http())?,
            model,
            default_voice,
            descriptor,
            default_settings: config.default_settings.clone(),
        })
    }

    /// Replaces the advertised voice catalogue.
    ///
    /// Entries whose ids are not usable as a path segment are dropped rather
    /// than offered, on the same terms as [`Self::load_voices`].
    #[must_use]
    pub fn with_voices(mut self, voices: Vec<Voice>) -> Self {
        self.descriptor.metadata.voices = voices
            .into_iter()
            .filter(|voice| crate::voice_id::validate(&voice.id).is_ok())
            .collect();
        self
    }

    /// Reads the account's voice catalogue and advertises it.
    ///
    /// Separate from [`Self::new`] because it is a round trip needing the
    /// credential, and a provider that cannot be constructed cannot report its
    /// own health. A caller with an async context asks for the menu; one
    /// without gets a provider that works and advertises no catalogue, which is
    /// what an empty [`Metadata::voices`] means.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if the catalogue cannot be fetched or read.
    pub async fn load_voices(&mut self) -> Result<&[Voice]> {
        let response = self.http.send(self.http.get("voices")).await?;
        let catalogue: crate::wire::Catalogue = response
            .json()
            .await
            .map_err(|error| self.http.body_failure("voice catalogue", error))?;

        let offered = catalogue.voices.len();
        self.descriptor.metadata.voices =
            catalogue.voices.iter().filter_map(crate::wire::CatalogueVoice::to_voice).collect();
        tracing::debug!(
            provider = %self.http.name(),
            offered,
            usable = self.descriptor.metadata.voices.len(),
            "read the voice catalogue"
        );
        Ok(&self.descriptor.metadata.voices)
    }

    /// The voice to speak with, most specific choice first.
    ///
    /// A request that names one gets it. Otherwise the configured voice is what
    /// the operator asked for, and the first catalogue entry after that. There
    /// is no hard-coded last resort: unlike OpenAI's six fixed names, an
    /// ElevenLabs voice id is account-scoped, so a built-in default would be a
    /// guess at another account's data.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] naming the `voice_id` field if the requested
    /// voice is not a value this crate will place in a URL path, or if no voice
    /// is available at all.
    fn voice_for(&self, requested: Option<&str>) -> Result<String> {
        if let Some(voice) = requested {
            // The one path that can carry an unchecked value at request time: a
            // pipeline node's `voice` setting. Checked before it is a URL.
            return crate::voice_id::validate(voice).map(str::to_owned);
        }
        if let Some(voice) = &self.default_voice {
            return Ok(voice.clone());
        }
        // Already validated on the way in, by `with_voices` or `load_voices`.
        self.descriptor.metadata.voices.first().map(|voice| voice.id.clone()).ok_or_else(|| {
            Error::Config(
                "no ElevenLabs voice is configured: set the `voice_id` field, or read the \
                     account catalogue. Voice ids are account-scoped, so there is no default \
                     that is safe to assume."
                    .to_owned(),
            )
        })
    }

    /// The voice controls for one request, from the stored defaults, the
    /// request's own settings, and its rate.
    fn voice_settings(&self, request: &SynthesisRequest) -> VoiceSettings {
        let settings =
            crate::layered_settings(&self.default_settings, request.settings.as_map());
        let number = |name: &str| settings.get(name).and_then(serde_json::Value::as_f64);
        VoiceSettings {
            stability: number("stability"),
            similarity_boost: number("similarity_boost"),
            style: number("style"),
            use_speaker_boost: settings
                .get("use_speaker_boost")
                .and_then(serde_json::Value::as_bool),
            // The request's rate, not a declared setting: `SynthesisRequest`
            // already has a field for this, and two ways to say it means one
            // silently loses.
            speed: request.rate,
        }
    }

    /// The `language_code` for one request, from the stored defaults and the
    /// request's own settings.
    fn language_code(&self, request: &SynthesisRequest) -> Option<String> {
        crate::layered_settings(&self.default_settings, request.settings.as_map())
            .get("language_code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }
}

#[async_trait::async_trait]
impl Provider for ElevenLabsTts {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        // The voice catalogue, because it is the cheapest call that exercises
        // the credential as well as the connection: `/v1/models` 404s and
        // nothing here is reachable unauthenticated, so a check that skipped the
        // key would report a rejected key as healthy.
        match self.http.send(self.http.get("voices")).await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl TextToSpeech for ElevenLabsTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        let voice = self.voice_for(request.voice.as_deref())?;
        let output = OutputFormat::for_request(request.format)?;
        if !output.honours(request.format) {
            // Not a failure — the first chunk reports what was produced, which
            // is the documented contract — but a pipeline resampling every
            // utterance is usually a misconfiguration worth seeing.
            tracing::info!(
                provider = %self.http.name(),
                requested = ?request.format,
                produced = ?output.produced,
                "ElevenLabs cannot produce the requested format; chunks report what it did"
            );
        }

        let body = Synthesis {
            voice_settings: self.voice_settings(&request),
            language_code: self.language_code(&request),
            text: request.text,
            model_id: self.model.clone(),
        };
        tracing::debug!(
            provider = %self.http.name(),
            model = %body.model_id,
            voice = %voice,
            output_format = %output.query.as_query(),
            "synthesizing"
        );

        // `voice` is in the path, so it has been through the allowlist. The
        // format is a query parameter rather than a body field here, unlike
        // every other provider in this workspace.
        let path = format!("text-to-speech/{voice}/stream");
        let response = self
            .http
            .send(
                self.http
                    .post(&path)
                    .query(&[("output_format", output.query.as_query())])
                    .json(&body),
            )
            .await?;

        // The endpoint sends raw PCM with no framing, so a chunk is however much
        // has arrived. Forwarding packets as they land is what lets playback
        // start before synthesis finishes — and dropping this stream drops the
        // response body, which closes the connection and stops synthesis, which
        // is how barge-in silences the assistant mid-sentence.
        let name = self.http.name().to_owned();
        let format = output.produced;
        // `unfold` rather than `map`, and the difference matters: a `reqwest`
        // body that has failed keeps reporting the same failure every time it is
        // polled, so mapping it would yield an endless stream of identical
        // errors. A caller draining a lost turn would spin forever instead of
        // moving on. Carrying the body in an `Option` ends the stream after the
        // one error that explains it.
        let state = Some((Box::pin(response.bytes_stream()), 0_u64));
        let chunks = futures_util::stream::unfold(state, move |state| {
            let name = name.clone();
            async move {
                let (mut body, sequence) = state?;
                match body.next().await? {
                    Ok(data) => {
                        let chunk = SpeechChunk { sequence, format, data };
                        Some((Ok(chunk), Some((body, sequence + 1))))
                    }
                    // Audio that stops arriving partway through becomes an error
                    // *item* rather than a clean end: a turn that lost its voice
                    // halfway must be distinguishable from one that finished
                    // speaking, or the pipeline waits for a reply to half a
                    // sentence.
                    Err(error) => {
                        Some((Err(Error::provider(&name, Failure::transport(&error))), None))
                    }
                }
            }
        });

        Ok(Box::pin(chunks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::audio::AudioFormat;

    fn voice(id: &str) -> Voice {
        Voice { id: id.to_owned(), name: id.to_owned(), language: "en".to_owned() }
    }

    fn synthesizer() -> ElevenLabsTts {
        ElevenLabsTts::new(&ElevenLabsConfig::default()).expect("client")
    }

    #[test]
    fn a_requested_voice_wins_over_the_configured_one() {
        let provider = ElevenLabsTts::new(&ElevenLabsConfig {
            voice_id: Some("21m00Tcm4TlvDq8ikWAM".to_owned()),
            ..Default::default()
        })
        .expect("client");

        assert_eq!(
            provider.voice_for(Some("9BWtsMINqrJLrRacOk9x")).expect("valid"),
            "9BWtsMINqrJLrRacOk9x"
        );
        assert_eq!(provider.voice_for(None).expect("valid"), "21m00Tcm4TlvDq8ikWAM");
    }

    #[test]
    fn the_catalogue_supplies_a_voice_when_nothing_else_does() {
        let provider = synthesizer().with_voices(vec![voice("abc123"), voice("def456")]);
        assert_eq!(provider.voice_for(None).expect("valid"), "abc123");
    }

    #[test]
    fn a_provider_with_no_voice_at_all_says_so_rather_than_guessing() {
        // Voice ids are account-scoped, so there is no equivalent of `alloy` to
        // fall back to: a hard-coded default would be a guess at another
        // account's data, and would 404 at best.
        let error = synthesizer().voice_for(None).expect_err("no voice");
        assert!(error.to_string().contains("voice_id"), "{error}");
    }

    #[test]
    fn a_requested_voice_that_is_a_traversal_attempt_is_refused_before_it_is_a_url() {
        // The guarantee: a pipeline node's `voice` setting is the one voice id
        // that reaches this provider unvalidated at request time.
        let provider = synthesizer().with_voices(vec![voice("abc123")]);
        let error = provider.voice_for(Some("../../v1/user")).expect_err("traversal");

        assert!(matches!(error, Error::Config(_)), "{error}");
        assert!(error.to_string().contains("voice_id"), "{error}");
    }

    #[test]
    fn a_configured_voice_that_is_a_traversal_attempt_refuses_to_become_a_provider() {
        // A stored definition carrying one must fail when it is saved, not on
        // the first turn.
        let error = ElevenLabsTts::new(&ElevenLabsConfig {
            voice_id: Some("../../user".to_owned()),
            ..Default::default()
        })
        .expect_err("traversal");

        assert!(matches!(error, Error::Config(_)), "{error}");
        assert!(error.to_string().contains("voice_id"), "{error}");
    }

    #[test]
    fn a_catalogue_voice_that_is_a_traversal_attempt_is_never_offered() {
        // Not merely refused at synthesis: it must not appear in the menu an
        // operator picks from, or the refusal looks like a bug in the product.
        let provider = synthesizer().with_voices(vec![voice("../../user"), voice("abc123")]);
        let ids: Vec<_> =
            provider.descriptor().metadata.voices.iter().map(|voice| &voice.id).collect();

        assert_eq!(ids, ["abc123"]);
    }

    #[test]
    fn the_descriptor_names_the_models_the_encodings_and_the_catalogue() {
        let provider = synthesizer().with_voices(vec![voice("abc123")]);
        let metadata = &provider.descriptor().metadata;

        assert_eq!(metadata.models, DEFAULT_TTS_MODELS);
        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(!metadata.supports_encoding(Encoding::Opus), "framing is unconfirmed");
        assert_eq!(metadata.voices, [voice("abc123")], "the catalogue is read, not awaited");
    }

    #[test]
    fn the_first_named_model_is_the_one_used() {
        let provider = ElevenLabsTts::new(&ElevenLabsConfig {
            models: vec!["eleven_v3".to_owned(), "eleven_flash_v2".to_owned()],
            ..Default::default()
        })
        .expect("client");

        assert_eq!(provider.model, "eleven_v3");
        assert_eq!(synthesizer().model, DEFAULT_TTS_MODEL, "and the default otherwise");
    }

    #[test]
    fn the_requests_rate_becomes_speed_rather_than_a_declared_setting() {
        // `SynthesisRequest` already has a field for this. Declaring `speed` as
        // well would send the same control twice.
        let provider = synthesizer();
        let request = SynthesisRequest { rate: Some(1.25), ..SynthesisRequest::new("hi") };

        assert_eq!(provider.voice_settings(&request).speed, Some(1.25));
        assert!(!settings_schema().as_json()["properties"]
            .as_object()
            .expect("properties")
            .contains_key("speed"));
    }

    #[test]
    fn unconfigured_voice_controls_are_left_to_the_voices_own_tuning() {
        let provider = synthesizer();
        let settings = provider.voice_settings(&SynthesisRequest::new("hi"));

        assert!(settings.is_empty(), "{settings:?}");
    }

    #[test]
    fn stored_defaults_supply_the_voice_controls_when_a_request_names_none() {
        let mut defaults = serde_json::Map::new();
        defaults.insert("stability".to_owned(), serde_json::json!(0.4));
        defaults.insert("use_speaker_boost".to_owned(), serde_json::json!(true));
        defaults.insert("language_code".to_owned(), serde_json::json!("de"));
        let provider = ElevenLabsTts::new(&ElevenLabsConfig {
            default_settings: defaults,
            ..Default::default()
        })
        .expect("client");

        let request = SynthesisRequest::new("hi");
        let settings = provider.voice_settings(&request);
        assert_eq!(settings.stability, Some(0.4));
        assert_eq!(settings.use_speaker_boost, Some(true));
        assert_eq!(provider.language_code(&request).as_deref(), Some("de"));
    }

    #[test]
    fn a_declared_setting_is_checked_against_its_documented_bounds() {
        // Refused here rather than becoming a 422 the operator sees mid-turn.
        let descriptor = synthesizer().descriptor().clone();
        for value in [
            serde_json::json!({ "stability": 1.5 }),
            serde_json::json!({ "similarity_boost": -0.1 }),
            serde_json::json!({ "style": "high" }),
            serde_json::json!({ "similarity-boost": 0.5 }),
            serde_json::json!({ "speed": 1.2 }),
        ] {
            assert!(descriptor.validate_settings(&value).is_err(), "{value} should be refused");
        }
        assert!(descriptor
            .validate_settings(&serde_json::json!({ "stability": 0.4, "style": 0.0 }))
            .is_ok());
    }

    #[test]
    fn the_default_audio_format_is_produced_verbatim() {
        let output = OutputFormat::for_request(AudioFormat::DEFAULT).expect("supported");
        assert!(output.honours(AudioFormat::DEFAULT), "the interchange format needs no work");
    }
}
