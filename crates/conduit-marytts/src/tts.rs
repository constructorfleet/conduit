//! Speech synthesis against a MaryTTS server.
//!
//! One request to `POST /process` returns the whole utterance as a WAV file.
//! See [`MaryTts::synthesize`] for what that means for the streaming contract.

use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::Result;
use conduit_http::Http;
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{
    Capability, ChunkStream, Descriptor, Health, Metadata, Provider, SettingsSchema,
};
use futures_util::stream;

use crate::validate;
use crate::MaryTtsConfig;

/// The one encoding this provider emits.
///
/// The server can be asked for MP3 or Vorbis, but the pipeline carries samples
/// and this crate has no decoder, so `WAVE_FILE` is the only format requested
/// and 16-bit PCM is the only thing produced.
const ENCODINGS: [Encoding; 1] = [Encoding::PcmS16Le];

/// The synthesis endpoint.
const PROCESS: &str = "process";

/// The voice catalogue.
const VOICES: &str = "voices";

/// The locales the server can speak.
const LOCALES: &str = "locales";

/// The cheapest endpoint that proves a MaryTTS server is answering.
const VERSION: &str = "version";

/// What the endpoint is asked for. `WAVE_FILE` rather than `WAVE_STREAM`
/// because MaryTTS only offers the `_STREAM` suffix for MP3 and Vorbis —
/// `MaryRuntimeUtils.getAudioFileFormatTypes()` appends it for those two types
/// alone — and neither is samples.
const WAVE_FILE: &str = "WAVE_FILE";

/// A synthesizer backed by a self-hosted MaryTTS server.
#[derive(Debug, Clone)]
pub struct MaryTts {
    http: Http,
    descriptor: Descriptor,
    /// Default voice, when the deployment configured one.
    voice: Option<String>,
    /// Locale sent when a request names no voice of its own.
    locale: String,
}

/// What `/process` accepts beyond a voice and a locale.
///
/// `STYLE` is a voice-specific label — MaryTTS reads the set a voice supports
/// from its own config — so it is declared as a free string rather than an
/// enum this crate would have to keep in step with every installed voice.
fn settings_schema() -> SettingsSchema {
    SettingsSchema::new(serde_json::json!({
        "type": "object",
        "properties": {
            "style": {
                "type": "string",
                "description":
                    "A speaking style the chosen voice defines, e.g. `poker` or `happy`. \
                     Voices that declare no styles ignore it.",
            },
        },
    }))
    .expect("a literal object schema")
}

impl MaryTts {
    /// Builds a synthesizer for the server `config` describes. Does not
    /// connect.
    ///
    /// The voice catalogue is not fetched here: constructing a provider is
    /// synchronous and must not depend on a server being up. Call
    /// [`with_catalogue`](Self::with_catalogue) to fill it in, or let
    /// [`refresh_catalogue`](Self::refresh_catalogue) read it from the server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built, or if the
    /// configured voice or locale is not one that may be put in a request.
    pub fn new(config: MaryTtsConfig) -> Result<Self> {
        // Validated at construction as well as per request, so a deployment
        // learns its configuration is wrong when it starts rather than the
        // first time somebody speaks.
        let locale = validate::locale(&config.name, &config.locale)?;
        let voice = config
            .voice
            .as_deref()
            .map(|voice| validate::voice(&config.name, voice))
            .transpose()?;

        let voices = voice
            .iter()
            .map(|id| Voice {
                id: id.clone(),
                name: id.clone(),
                language: locale.replace('_', "-"),
            })
            .collect();

        let descriptor = Descriptor::new(config.name.clone(), Capability::Tts)
            .with_label(config.label.clone().unwrap_or_else(|| config.name.clone()))
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_metadata(
                Metadata::default()
                    .with_voices(voices)
                    .with_languages(vec![locale.replace('_', "-")])
                    .with_encodings(ENCODINGS.to_vec()),
            )
            .with_settings(settings_schema());

        Ok(Self { http: Http::new(config.into_http())?, descriptor, voice, locale })
    }

    /// Replaces the advertised voice catalogue and the languages it covers.
    ///
    /// What [`refresh_catalogue`](Self::refresh_catalogue) does with a live
    /// server, separated out so a caller that already knows the catalogue — a
    /// stored provider record, a test — does not need one.
    #[must_use]
    pub fn with_catalogue(mut self, voices: Vec<Voice>) -> Self {
        self.descriptor.metadata.languages = languages_of(&voices);
        self.descriptor.metadata.voices = voices;
        self
    }

    /// Reads the server's voice catalogue and adopts it.
    ///
    /// `GET /voices` is the only way to know what a MaryTTS install actually
    /// has: voices are dropped in as jars, so the list is per-deployment and
    /// cannot be a constant in this crate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if the server cannot be reached or answers
    /// with a status outside 2xx. The catalogue is left as it was.
    pub async fn refresh_catalogue(&mut self) -> Result<&[Voice]> {
        let voices = crate::catalogue::voices(&self.text(VOICES).await?);
        tracing::debug!(provider = %self.name(), voices = voices.len(), "read the catalogue");
        self.descriptor.metadata.languages = languages_of(&voices);
        self.descriptor.metadata.voices = voices;
        Ok(&self.descriptor.metadata.voices)
    }

    /// The locales the server says it can speak, as BCP-47 tags.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if the server cannot be reached or answers
    /// with a status outside 2xx.
    pub async fn locales(&self) -> Result<Vec<String>> {
        Ok(crate::catalogue::locales(&self.text(LOCALES).await?))
    }

    /// The voices this synthesizer advertises.
    #[must_use]
    pub fn voices(&self) -> &[Voice] {
        &self.descriptor.metadata.voices
    }

    /// GETs `path` and reads the plain-text body.
    async fn text(&self, path: &str) -> Result<String> {
        let response = self.http.send(self.http.get(path)).await?;
        response.text().await.map_err(|error| self.http.body_failure(path, error))
    }

    /// The voice to speak with, most specific choice first.
    ///
    /// A request that names one gets it. Otherwise the configured voice is what
    /// the operator chose. `None` leaves `VOICE` off the request entirely, which
    /// is how the server is asked for its own default for the locale — better
    /// than this crate inventing a name, since no voice ships with MaryTTS
    /// itself.
    fn voice_for(&self, requested: Option<String>) -> Result<Option<String>> {
        match requested.or_else(|| self.voice.clone()) {
            Some(voice) => validate::voice(self.name(), &voice).map(Some),
            None => Ok(None),
        }
    }

    /// The locale to synthesize in.
    ///
    /// A voice determines its own locale, so this is the configured one: sending
    /// a locale that disagrees with the voice is how a MaryTTS request gets
    /// rejected. Kept because `LOCALE` is required whenever no voice is named.
    fn locale_for(&self, voice: Option<&str>) -> Result<String> {
        // Where a request named a voice from the catalogue, its own language is
        // more trustworthy than the provider's default locale.
        let from_catalogue = voice.and_then(|voice| {
            self.voices()
                .iter()
                .find(|candidate| candidate.id == voice)
                .map(|candidate| candidate.language.clone())
                .filter(|language| !language.is_empty())
        });
        match from_catalogue {
            Some(language) => validate::locale(self.name(), &language),
            None => Ok(self.locale.clone()),
        }
    }
}

/// The distinct languages a catalogue covers, in the order first seen.
fn languages_of(voices: &[Voice]) -> Vec<String> {
    let mut languages: Vec<String> = Vec::new();
    for voice in voices {
        if !voice.language.is_empty() && !languages.contains(&voice.language) {
            languages.push(voice.language.clone());
        }
    }
    languages
}

#[async_trait::async_trait]
impl Provider for MaryTts {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Asks the server for its version.
    ///
    /// A real request to a real endpoint, so a server that is down, wedged, or
    /// behind a proxy that is refusing reports [`Health::Unhealthy`] with the
    /// reason. `GET /version` is chosen over `/voices` because it is the
    /// cheapest thing the server will answer and it does not depend on any
    /// voice being installed.
    async fn health(&self) -> Health {
        match self.text(VERSION).await {
            // The body is `Mary TTS server 5.2 (impl. 5.2)`. A 200 with an
            // empty body is something else answering on that port.
            Ok(version) if version.trim().is_empty() => Health::Degraded {
                reason: "the server answered /version with an empty body".to_owned(),
            },
            Ok(version) => {
                tracing::debug!(provider = %self.name(), version = %version.trim(), "healthy");
                Health::Healthy
            }
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl TextToSpeech for MaryTts {
    /// Synthesizes `request` in one round trip.
    ///
    /// # Streaming
    ///
    /// This is honest about being unstreamed. `/process` with `WAVE_FILE`
    /// computes the whole utterance and returns it as one file — there is no
    /// framing to forward incrementally and no `WAVE_STREAM` to ask for — so the
    /// returned stream yields exactly one [`SpeechChunk`] once synthesis has
    /// finished. Time to first audio is therefore the time to synthesize the
    /// entire utterance, and it grows with the length of the reply. A caller
    /// that needs speech to begin before the sentence is finished should split
    /// the text upstream and synthesize the parts, which is a decision this
    /// provider deliberately does not make on its behalf.
    ///
    /// The chunk's format is always [`AudioFormat::DEFAULT`], whatever rate the
    /// voice was built at: [`crate::audio`] reads the real rate from the WAV
    /// header and converts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the text is empty or if the voice or locale
    /// is not one that may be put in a request, and [`Error::Provider`] if the
    /// server rejects the request or cannot be reached. A failure that happens
    /// while reading or decoding the body arrives as an error item on the
    /// stream, so a caller sees a failed turn rather than a silent one.
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        validate::text(self.name(), &request.text)?;
        let voice = self.voice_for(request.voice)?;
        let locale = self.locale_for(voice.as_deref())?;

        // A form body rather than a query string, which `/process` accepts:
        // `BaseHttpRequestHandler` parses a POST entity as URL-encoded
        // parameters when the URI carries no query of its own. It matters
        // because the utterance is the one parameter with no length bound, and
        // a transform-heavy pipeline produces long ones — servers and proxies
        // cap a URL somewhere around 4–8 KB, so a query string would truncate a
        // long reply or fail it outright. `reqwest` percent-encodes each value,
        // so the text needs no escaping here.
        let mut form: Vec<(&str, String)> = vec![
            ("INPUT_TEXT", request.text),
            ("INPUT_TYPE", "TEXT".to_owned()),
            ("OUTPUT_TYPE", "AUDIO".to_owned()),
            ("AUDIO", WAVE_FILE.to_owned()),
            ("LOCALE", locale),
        ];
        if let Some(voice) = &voice {
            form.push(("VOICE", voice.clone()));
        }
        // Validated against the schema above before it reached here.
        if let Some(style) = request.settings.as_map().get("style").and_then(|v| v.as_str()) {
            form.push(("STYLE", style.to_owned()));
        }

        tracing::debug!(
            provider = %self.name(),
            voice = voice.as_deref().unwrap_or("<server default>"),
            characters = form[0].1.len(),
            "synthesizing"
        );

        let response = self.http.send(self.http.post(PROCESS).form(&form)).await?;

        let name = self.name().to_owned();
        // Read whole because the payload is a WAV file, and a file cannot be
        // decoded from its first packet. This is the buffering the doc comment
        // above owns rather than hides.
        let chunk = match response.bytes().await {
            Ok(body) => crate::audio::to_interchange(&name, &body).map(|samples| SpeechChunk {
                sequence: 0,
                format: AudioFormat::DEFAULT,
                data: Bytes::from(samples),
            }),
            // The server accepted the request and then stopped sending. That is
            // a mid-stream failure, and it belongs on the stream as an error
            // item: a caller that got an empty stream instead would render the
            // turn as the assistant having nothing to say. `body_failure`
            // keeps a stall retryable and a body that arrived wrong not.
            Err(error) => Err(self.http.body_failure("synthesized audio", error)),
        };

        Ok(Box::pin(stream::once(async move { chunk })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::Error;

    fn config() -> MaryTtsConfig {
        MaryTtsConfig { base_url: "http://localhost:59125".to_owned(), ..Default::default() }
    }

    fn provider() -> MaryTts {
        MaryTts::new(config()).expect("builds")
    }

    fn voice(id: &str, language: &str) -> Voice {
        Voice { id: id.to_owned(), name: id.to_owned(), language: language.to_owned() }
    }

    #[test]
    fn a_provider_is_built_without_reaching_the_server() {
        // Construction is synchronous and a server may be down at start-up.
        let provider = provider();
        assert_eq!(provider.name(), "marytts");
        assert_eq!(provider.locale, "en_US");
        assert!(provider.voice.is_none());
    }

    #[test]
    fn only_sixteen_bit_pcm_is_advertised() {
        let provider = provider();
        let metadata = &provider.descriptor().metadata;
        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(!metadata.supports_encoding(Encoding::Opus));
        assert!(!metadata.supports_encoding(Encoding::PcmF32Le));
    }

    #[test]
    fn a_configured_voice_that_could_be_injected_is_refused_at_construction() {
        // The guarantee: a deployment learns its configuration is unsafe when
        // it starts, not the first time somebody speaks.
        let error = MaryTts::new(MaryTtsConfig {
            voice: Some("cmu-slt-hsmm&OUTPUT_TYPE=TEXT".to_owned()),
            ..config()
        })
        .expect_err("refused");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains("`voice`"), "{error}");
    }

    #[test]
    fn a_configured_locale_that_could_be_injected_is_refused_at_construction() {
        let error =
            MaryTts::new(MaryTtsConfig { locale: "en_US&VOICE=x".to_owned(), ..config() })
                .expect_err("refused");
        assert!(error.to_string().contains("`locale`"), "{error}");
    }

    #[test]
    fn a_requested_voice_wins_over_the_configured_one() {
        let provider =
            MaryTts::new(MaryTtsConfig { voice: Some("cmu-slt-hsmm".to_owned()), ..config() })
                .expect("builds");
        assert_eq!(
            provider.voice_for(Some("dfki-prudence-hsmm".to_owned())).expect("valid"),
            Some("dfki-prudence-hsmm".to_owned())
        );
    }

    #[test]
    fn the_configured_voice_is_used_when_a_request_names_none() {
        let provider =
            MaryTts::new(MaryTtsConfig { voice: Some("cmu-slt-hsmm".to_owned()), ..config() })
                .expect("builds");
        assert_eq!(provider.voice_for(None).expect("valid"), Some("cmu-slt-hsmm".to_owned()));
    }

    #[test]
    fn no_voice_at_all_lets_the_server_choose_its_own() {
        // No voice ships with MaryTTS itself, so inventing a name here would
        // guess wrong on every install.
        assert_eq!(provider().voice_for(None).expect("valid"), None);
    }

    #[test]
    fn a_requested_voice_that_could_be_injected_is_refused_before_the_request() {
        let error =
            provider().voice_for(Some("x&AUDIO=MP3_FILE".to_owned())).expect_err("refused");
        assert!(error.to_string().contains("`voice`"), "{error}");
    }

    #[test]
    fn a_voice_from_the_catalogue_brings_its_own_locale() {
        // Sending a locale that disagrees with the voice is how MaryTTS
        // rejects a request.
        let provider = provider().with_catalogue(vec![voice("bits1-hsmm", "de")]);
        assert_eq!(provider.locale_for(Some("bits1-hsmm")).expect("valid"), "de");
    }

    #[test]
    fn a_voice_the_catalogue_does_not_know_falls_back_to_the_configured_locale() {
        let provider = provider().with_catalogue(vec![voice("bits1-hsmm", "de")]);
        assert_eq!(provider.locale_for(Some("something-else")).expect("valid"), "en_US");
        assert_eq!(provider.locale_for(None).expect("valid"), "en_US");
    }

    #[test]
    fn a_catalogue_declares_the_languages_it_covers_without_repeating_them() {
        let provider = provider().with_catalogue(vec![
            voice("cmu-slt-hsmm", "en-US"),
            voice("cmu-bdl-hsmm", "en-US"),
            voice("bits1-hsmm", "de"),
            voice("nameless", ""),
        ]);

        assert_eq!(provider.descriptor().metadata.languages, ["en-US", "de"]);
        assert_eq!(provider.voices().len(), 4);
    }

    #[test]
    fn the_configured_locale_is_advertised_before_a_catalogue_is_read() {
        // An operator screen has something to show without a live server.
        let provider = MaryTts::new(MaryTtsConfig { locale: "de".to_owned(), ..config() })
            .expect("builds");
        assert_eq!(provider.descriptor().metadata.languages, ["de"]);
    }

    #[test]
    fn a_bcp_47_locale_is_normalized_for_the_wire_and_reported_as_bcp_47() {
        let provider = MaryTts::new(MaryTtsConfig { locale: "en-GB".to_owned(), ..config() })
            .expect("builds");
        assert_eq!(provider.locale, "en_GB", "the wire form");
        assert_eq!(provider.descriptor().metadata.languages, ["en-GB"], "the reported form");
    }

    #[test]
    fn a_style_is_the_only_setting_declared() {
        let descriptor = provider().descriptor().clone();
        assert!(descriptor.validate_settings(&serde_json::json!({ "style": "poker" })).is_ok());
        // `additionalProperties` defaults to false, so a typo is an error
        // rather than a setting the server silently ignores.
        assert!(descriptor.validate_settings(&serde_json::json!({ "styl": "poker" })).is_err());
    }

    #[test]
    fn the_label_is_what_an_operator_reads_and_the_name_is_what_a_pipeline_selects() {
        let provider = MaryTts::new(MaryTtsConfig {
            name: "marytts-kitchen".to_owned(),
            label: Some("MaryTTS (kitchen)".to_owned()),
            ..config()
        })
        .expect("builds");

        assert_eq!(provider.name(), "marytts-kitchen");
        assert_eq!(provider.descriptor().label, "MaryTTS (kitchen)");
    }

    #[test]
    fn the_label_defaults_to_the_identity() {
        assert_eq!(provider().descriptor().label, "marytts");
    }
}
