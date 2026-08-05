//! Speech synthesis against Deepgram's `/v1/speak`.

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};
use conduit_http::Http;
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{Capability, ChunkStream, Descriptor, Health, Metadata, Provider};
use futures_util::StreamExt;
use serde::Serialize;

use crate::{model_id, DeepgramTtsConfig, DEFAULT_MODEL};

/// The synthesis endpoint.
const SPEAK: &str = "speak";

/// The encodings this provider will ask for.
///
/// `linear16` is raw PCM and the pipeline's own interchange format. Deepgram
/// also offers mp3, aac, opus, flac, mulaw, and alaw; the ones this crate has no
/// decoder for are not advertised, because a provider that reports an encoding
/// it cannot hand the pipeline as samples is a provider that fails a turn after
/// synthesis has already been paid for.
const ENCODINGS: [Encoding; 2] = [Encoding::PcmS16Le, Encoding::Flac];

/// The sample rates `linear16` accepts.
///
/// Rejecting an unsupported rate here rather than passing it through: the API
/// answers a bad rate with a 400 naming its own parameter, and an operator
/// reading that has to work out that Conduit chose the number. Naming the
/// permitted set is the difference between a message they can act on and one
/// they cannot.
const LINEAR16_SAMPLE_RATES: [u32; 5] = [8_000, 16_000, 24_000, 32_000, 48_000];

/// The longest text one request may carry.
///
/// Deepgram caps Aura input, and the cap is enforced here so the failure names
/// the limit and the actual length. The alternative — splitting long text across
/// requests and concatenating — is not silently correct: each request is
/// synthesized independently, so the prosody restarts at every seam and the
/// result sounds like separate sentences read by the same voice. That may be an
/// acceptable trade, but it is a decision for a caller that can see the text,
/// not one to make invisibly inside a provider.
const MAX_CHARACTERS: usize = 2_000;

/// A synthesizer backed by Deepgram Aura.
#[derive(Debug, Clone)]
pub struct DeepgramTts {
    http: Http,
    descriptor: Descriptor,
    /// The Aura model, which is also the voice.
    model: String,
}

/// The request body, which carries only the text.
///
/// Everything else — the model, the encoding, the container, the sample rate —
/// is a query parameter. That split is the vendor's, not a choice here.
#[derive(Debug, Serialize)]
struct Request {
    text: String,
}

impl DeepgramTts {
    /// Builds a synthesizer from `config`. Does not connect.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the HTTP client cannot be built, or if the
    /// configured model is not one that may be put in a request.
    pub fn new(config: DeepgramTtsConfig) -> Result<Self> {
        // Validated at construction as well as per request, so a deployment
        // learns its configuration is wrong when it starts rather than the first
        // time somebody speaks.
        let model = match config.model.clone() {
            Some(model) => model_id::validate(&model)?.to_owned(),
            None => DEFAULT_MODEL.to_owned(),
        };
        let name = config.name.clone();
        let label = config.label.clone().unwrap_or_else(|| name.clone());

        let descriptor = Descriptor::new(name, Capability::Tts)
            .with_label(label)
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_metadata(
                Metadata::default()
                    .with_models(vec![model.clone()])
                    // The voice *is* the model, so the catalogue holds the one
                    // configured id rather than the full Aura list. Deepgram
                    // publishes no endpoint to enumerate voices, and a hardcoded
                    // list would be a copy of a web page that goes stale
                    // silently — an operator offered a voice the API has since
                    // renamed gets a 400 from a menu Conduit gave them.
                    .with_voices(vec![Voice {
                        id: model.clone(),
                        name: model.clone(),
                        language: language_of(&model),
                    }])
                    .with_languages(vec![language_of(&model)])
                    .with_encodings(ENCODINGS.to_vec()),
            );
        // No `with_settings`: `/v1/speak` takes a voice, an encoding, and a
        // rate, all of which are already fields on a request. An empty settings
        // schema would render as an empty box in the console.

        Ok(Self { http: Http::new(config.into_http())?, descriptor, model })
    }

    /// The model a request should use: the one it named, or the configured one.
    fn model_for(&self, requested: Option<String>) -> String {
        requested.unwrap_or_else(|| self.model.clone())
    }
}

/// The language an Aura id speaks, read off its own suffix.
///
/// Aura ids are `[family]-[voice]-[language]`, so `aura-2-thalia-en` speaks
/// `en`. Derived rather than configured because the id already states it, and a
/// separate language field could contradict the voice it describes. An id that
/// does not follow the convention reports no language rather than a guessed one.
fn language_of(model: &str) -> String {
    model.rsplit('-').next().filter(|tag| is_language_tag(tag)).unwrap_or("").to_owned()
}

/// Whether `tag` looks like the language suffix of an Aura id rather than part
/// of a voice name.
fn is_language_tag(tag: &str) -> bool {
    let length = tag.chars().count();
    (2..=3).contains(&length) && tag.chars().all(|character| character.is_ascii_lowercase())
}

/// The `encoding` value for `format`, and whether it needs `container=none`.
///
/// # Errors
///
/// Returns [`Error::Config`] for an encoding Deepgram does not produce, or for a
/// sample rate the chosen encoding does not offer.
fn wire_format(format: AudioFormat) -> Result<Vec<(&'static str, String)>> {
    let encoding = match format.encoding {
        Encoding::PcmS16Le => "linear16",
        Encoding::Flac => "flac",
        Encoding::Opus => {
            // Deepgram fixes Opus at 48 kHz inside an Ogg container, so it is
            // neither the rate the pipeline asked for nor bare samples.
            return Err(Error::Config(
                "Deepgram serves Opus only at 48 kHz in an Ogg container; ask for PcmS16Le"
                    .to_owned(),
            ));
        }
        Encoding::PcmF32Le => {
            return Err(Error::Config(
                "Deepgram does not produce 32-bit float PCM; ask for PcmS16Le".to_owned(),
            ));
        }
        // `Encoding` is non-exhaustive; a format this code predates is one the
        // API was never asked for.
        other => {
            return Err(Error::Config(format!("Deepgram does not produce {other:?}")));
        }
    };

    if !LINEAR16_SAMPLE_RATES.contains(&format.sample_rate) {
        return Err(Error::Config(format!(
            "Deepgram does not synthesize at {} Hz; it offers {}",
            format.sample_rate,
            LINEAR16_SAMPLE_RATES.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
        )));
    }

    Ok(vec![
        ("encoding", encoding.to_owned()),
        // `container=none` is the load-bearing one. The parameter defaults to
        // `wav`, so leaving it unset ships a 44-byte RIFF header into a stream
        // the pipeline treats as samples — which is not an error anywhere, just
        // a click at the start of every utterance and a length that is always
        // 44 bytes long.
        ("container", "none".to_owned()),
        ("sample_rate", format.sample_rate.to_string()),
    ])
}

#[async_trait::async_trait]
impl Provider for DeepgramTts {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        // Deepgram publishes no unauthenticated ping, and synthesizing to check
        // would bill the deployment for audio nobody hears. The shortest
        // billable utterance is the honest probe: it proves the key, the scheme,
        // and the model id together, which is exactly the set of things that go
        // wrong.
        let query = match wire_format(AudioFormat::DEFAULT) {
            Ok(mut query) => {
                query.push(("model", self.model.clone()));
                query
            }
            Err(error) => return Health::Unhealthy { reason: error.to_string() },
        };
        let request =
            self.http.post(SPEAK).query(&query).json(&Request { text: ".".to_owned() });
        match self.http.send(request).await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl TextToSpeech for DeepgramTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        let characters = request.text.chars().count();
        if characters > MAX_CHARACTERS {
            return Err(Error::Config(format!(
                "Deepgram accepts at most {MAX_CHARACTERS} characters per request, \
                 and this utterance is {characters}; split it upstream, where the \
                 sentence boundaries are known"
            )));
        }

        // A request may name a voice the definition did not, so it is checked
        // here too rather than trusted for having come from a caller.
        let model = self.model_for(request.voice);
        model_id::validate(&model)?;
        let mut query = wire_format(request.format)?;
        query.push(("model", model.clone()));

        tracing::debug!(model = %model, characters, "synthesizing");

        let response = self
            .http
            .send(self.http.post(SPEAK).query(&query).json(&Request { text: request.text }))
            .await?;

        let name = self.http.name().to_owned();
        let format = request.format;

        // `unfold` over an `Option` rather than `map` over the body, for the
        // reason `conduit-openai` records: a failed `reqwest` body re-reports the
        // same error on every poll, so a plain `map` yields an unbounded stream
        // of identical errors and a consumer draining until the end never
        // finishes. Taking the body out of the `Option` on the first failure
        // ends the stream after reporting it once.
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
                        Some(Err(error)) => {
                            let failure = conduit_http::Failure::transport(&error);
                            Some((Err(Error::provider(name, failure)), (None, sequence)))
                        }
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

    #[test]
    fn a_language_is_read_off_the_model_id() {
        assert_eq!(language_of("aura-2-thalia-en"), "en");
        assert_eq!(language_of("aura-2-celeste-es"), "es");
        assert_eq!(language_of("aura-asteria-en"), "en");
    }

    #[test]
    fn an_id_that_does_not_end_in_a_language_reports_none() {
        // Better than guessing: a made-up tag in the catalogue would be shown to
        // an operator as a fact about a voice.
        assert_eq!(language_of("aura-2-thalia"), "");
        assert_eq!(language_of("custom-model-NAME"), "");
    }

    #[test]
    fn raw_samples_are_asked_for_without_a_container() {
        let query = wire_format(AudioFormat::DEFAULT).expect("the interchange format");

        assert!(query.contains(&("encoding", "linear16".to_owned())));
        assert!(
            query.contains(&("container", "none".to_owned())),
            "the parameter defaults to `wav`, which would prepend a RIFF header \
             to a stream of samples"
        );
        assert!(query.contains(&("sample_rate", "16000".to_owned())));
    }

    #[test]
    fn a_sample_rate_deepgram_does_not_offer_is_refused_by_name() {
        let format = AudioFormat { sample_rate: 44_100, ..AudioFormat::DEFAULT };

        let error = wire_format(format).expect_err("44.1 kHz is not on the list");

        let message = error.to_string();
        assert!(message.contains("44100"), "the message names the rate asked for: {message}");
        assert!(message.contains("16000"), "and the rates on offer: {message}");
    }

    #[test]
    fn float_pcm_is_refused_with_the_encoding_to_ask_for_instead() {
        let format = AudioFormat { encoding: Encoding::PcmF32Le, ..AudioFormat::DEFAULT };

        let error = wire_format(format).expect_err("float PCM is not produced");

        assert!(error.to_string().contains("PcmS16Le"));
    }

    #[test]
    fn opus_is_refused_because_its_rate_and_container_are_both_fixed() {
        let format = AudioFormat { encoding: Encoding::Opus, sample_rate: 48_000, channels: 1 };

        let error = wire_format(format).expect_err("Opus arrives in an Ogg container");

        assert!(error.to_string().contains("Ogg"));
    }

    #[test]
    fn a_request_names_the_configured_model_when_it_asks_for_no_voice() {
        let tts = DeepgramTts::new(DeepgramTtsConfig {
            model: Some("aura-2-thalia-en".to_owned()),
            ..DeepgramTtsConfig::default()
        })
        .expect("builds");

        assert_eq!(tts.model_for(None), "aura-2-thalia-en");
        assert_eq!(tts.model_for(Some("aura-2-apollo-en".to_owned())), "aura-2-apollo-en");
    }

    #[test]
    fn the_default_model_is_the_one_the_api_would_have_chosen() {
        let tts = DeepgramTts::new(DeepgramTtsConfig::default()).expect("builds");

        // Stated rather than left to the server, so the descriptor reports the
        // voice a turn will actually use rather than an empty catalogue.
        assert_eq!(tts.model_for(None), DEFAULT_MODEL);
        assert_eq!(tts.descriptor().metadata.voices[0].id, DEFAULT_MODEL);
    }

    #[test]
    fn a_key_never_prints_itself() {
        // The provider derives `Debug`, so anything it prints can reach a log.
        let tts = DeepgramTts::new(DeepgramTtsConfig {
            api_key: Some("dg-secret".to_owned()),
            ..DeepgramTtsConfig::default()
        })
        .expect("builds");

        let printed = format!("{tts:?}");

        assert!(!printed.contains("dg-secret"), "a logged key is a leaked key: {printed}");
    }
}
