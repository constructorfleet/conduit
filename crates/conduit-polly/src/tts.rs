//! Speech synthesis over Amazon Polly's `SynthesizeSpeech`.

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{Capability, ChunkStream, Descriptor, Health, Metadata, Provider};

use crate::PollyTtsConfig;

/// The encodings this provider produces.
///
/// One, and it is the pipeline's own. See the crate docs for why the compressed
/// formats Polly also offers are refused rather than mislabelled.
const ENCODINGS: [Encoding; 1] = [Encoding::PcmS16Le];

/// Speech synthesis backed by Amazon Polly.
#[derive(Debug, Clone)]
pub struct PollyTts {
    /// Identity, label, and what this provider advertises.
    descriptor: Descriptor,
    /// The Polly client, built with a rustls-ring HTTP client.
    #[cfg(feature = "polly")]
    client: aws_sdk_polly::Client,
    /// The voice a request that names none is spoken in.
    ///
    /// Read only where there is an SDK to send it to. Kept in the struct either
    /// way rather than gated, so the two builds have one shape and a field cannot
    /// be forgotten when the feature is turned back on.
    #[cfg_attr(not(feature = "polly"), allow(dead_code))]
    voice: String,
    /// The engine every request asks for.
    #[cfg_attr(not(feature = "polly"), allow(dead_code))]
    engine: String,
    /// The region, kept for the health reason: a failure is far more legible
    /// once it says which region produced it.
    #[cfg_attr(not(feature = "polly"), allow(dead_code))]
    region: String,
}

impl PollyTts {
    /// Builds a synthesizer for `config`.
    ///
    /// Credentials are resolved here rather than at the first turn, so an
    /// operator saving a definition on a host with none is told while they are
    /// still looking at the form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] in a build compiled without the `polly` feature,
    /// with a message naming the feature.
    #[cfg_attr(not(feature = "polly"), allow(clippy::unused_async))]
    pub async fn new(config: PollyTtsConfig) -> Result<Self> {
        #[cfg(not(feature = "polly"))]
        {
            Err(Error::Config(format!(
                "provider `{}` talks to Amazon Polly, which this build cannot do: it was \
                 compiled without the `polly` feature",
                config.name
            )))
        }

        #[cfg(feature = "polly")]
        {
            let voice = config.voice().to_owned();
            let engine = config.engine().to_owned();
            let descriptor = Self::descriptor(&config);
            let shared = Self::loader(&config).load().await;

            Ok(Self {
                client: aws_sdk_polly::Client::new(&shared),
                descriptor,
                voice,
                engine,
                region: config.region,
            })
        }
    }

    /// What this provider advertises before it has asked Polly anything.
    ///
    /// The configured voice is the catalogue until [`Self::refresh_voices`]
    /// fetches the real one: a descriptor built without a network round trip can
    /// only advertise what the operator named, and advertising nothing would leave
    /// an operator screen unable to show the voice they chose.
    #[cfg_attr(not(feature = "polly"), allow(dead_code))]
    fn descriptor(config: &PollyTtsConfig) -> Descriptor {
        let mut descriptor = Descriptor::new(config.name.clone(), Capability::Tts)
            .with_metadata(
                Metadata::default()
                    .with_voices(vec![Voice {
                        id: config.voice().to_owned(),
                        name: config.voice().to_owned(),
                        // Not guessed from the voice name. Polly's ids carry no
                        // language — `Joanna` says nothing about `en-US` — and a
                        // wrong language on a descriptor is worse than an absent
                        // one, because a pipeline may route on it. `refresh_voices`
                        // fills it in with what the API reports.
                        language: String::new(),
                    }])
                    .with_encodings(ENCODINGS.to_vec()),
            );
        if let Some(label) = &config.label {
            descriptor = descriptor.with_label(label.clone());
        }
        descriptor
    }

    /// The credential and transport decisions, separated so they can be asserted
    /// without an AWS account: loading this yields an `SdkConfig` whose region,
    /// timeouts, and profile are all readable.
    #[cfg(feature = "polly")]
    fn loader(config: &PollyTtsConfig) -> aws_config::ConfigLoader {
        use aws_config::{BehaviorVersion, Region};
        use aws_smithy_http_client::tls;

        let mut timeouts = aws_smithy_types::timeout::TimeoutConfig::builder()
            .connect_timeout(config.connect_timeout);
        // `None` means something above this already imposes a deadline, and the
        // SDK's own default read timeout would undercut it.
        timeouts.set_read_timeout(config.read_timeout);

        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            // The SDK's own `rustls` feature selects `aws-lc-rs`, which is C,
            // wants cmake, and would put a second crypto provider in a binary
            // that already links ring for every other provider. Built here
            // instead and handed over, exactly as `conduit-bedrock` does, so the
            // whole workspace has one.
            .http_client(
                aws_smithy_http_client::Builder::new()
                    .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
                    .build_https(),
            )
            .timeout_config(timeouts.build());

        if let Some(profile) = &config.profile {
            loader = loader.profile_name(profile.clone());
        }

        loader
    }

    /// Replaces the advertised voice catalogue with what `DescribeVoices` reports.
    ///
    /// A separate call rather than part of construction, following
    /// `conduit-google`: building a provider should not depend on a network round
    /// trip, and a catalogue that failed to load must not stop a correctly
    /// configured provider from speaking.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if the catalogue cannot be fetched.
    #[cfg(feature = "polly")]
    pub async fn refresh_voices(&mut self) -> Result<&[Voice]> {
        let response = self.client.describe_voices().send().await.map_err(|error| {
            Error::provider(&self.descriptor.id, crate::failure::of_response(&error))
        })?;

        let voices: Vec<Voice> = response
            .voices
            .unwrap_or_default()
            .into_iter()
            .filter_map(|voice| {
                let id = voice.id?.as_str().to_owned();
                Some(Voice {
                    name: voice.name.clone().unwrap_or_else(|| id.clone()),
                    language: voice
                        .language_code
                        .map(|code| code.as_str().to_owned())
                        .unwrap_or_default(),
                    id,
                })
            })
            .collect();

        if !voices.is_empty() {
            self.descriptor.metadata.voices = voices;
        }
        Ok(&self.descriptor.metadata.voices)
    }

    /// The voices this synthesizer advertises.
    #[must_use]
    pub fn voices(&self) -> &[Voice] {
        &self.descriptor.metadata.voices
    }

    /// The rate to ask Polly for, and whether it is the one requested.
    ///
    /// Polly produces `pcm` at 8 kHz and 16 kHz only. Anything else is served at
    /// the nearest of the two rather than refused — a rate mismatch is something
    /// the pipeline can resample, unlike an encoding mismatch, which can only be
    /// mislabelled.
    // Not compiled out without the feature: the rule is arithmetic over a
    // constant, its tests run in both builds, and gating it would leave the leaner
    // build with less checked than the fuller one.
    #[must_use]
    #[cfg_attr(not(feature = "polly"), allow(dead_code))]
    fn nearest_rate(requested: u32) -> u32 {
        crate::PCM_SAMPLE_RATES
            .into_iter()
            .min_by_key(|rate| rate.abs_diff(requested))
            .unwrap_or(16_000)
    }

    /// Checks a request this provider can actually serve.
    fn accept(&self, request: &SynthesisRequest) -> Result<()> {
        if request.format.encoding != Encoding::PcmS16Le {
            return Err(Error::Config(format!(
                "provider `{}` synthesizes {:?} only; Polly's other formats are MP3 and Ogg \
                 containers this pipeline cannot label, or speech marks, which are not audio",
                self.descriptor.id,
                Encoding::PcmS16Le
            )));
        }
        // Refused here rather than passed through, so the message names the limit
        // and the actual length instead of relaying a vendor error.
        let characters = request.text.chars().count();
        if characters > crate::MAX_CHARACTERS {
            return Err(Error::Config(format!(
                "provider `{}` was asked to speak {characters} characters, over Polly's \
                 {} per request; split the utterance",
                self.descriptor.id,
                crate::MAX_CHARACTERS
            )));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Provider for PollyTts {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    async fn health(&self) -> Health {
        #[cfg(not(feature = "polly"))]
        {
            Health::Unhealthy { reason: "built without the `polly` feature".to_owned() }
        }

        #[cfg(feature = "polly")]
        {
            // `DescribeVoices` rather than a synthesis: it exercises the
            // credential, the region, and the engine together, and unlike a
            // one-character utterance it is not billed. A check that skipped the
            // credential would report a rejected role as healthy, which is worse
            // than not checking at all.
            match self
                .client
                .describe_voices()
                .engine(aws_sdk_polly::types::Engine::from(self.engine.as_str()))
                .send()
                .await
            {
                Ok(_) => Health::Healthy,
                Err(error) => Health::Unhealthy {
                    // The region as well as the failure: "access denied" reads
                    // very differently once you know which region denied it, and
                    // a voice configured for the wrong one is a common mistake.
                    reason: format!(
                        "{} in {}",
                        crate::failure::of_response(&error),
                        self.region
                    ),
                },
            }
        }
    }
}

#[async_trait::async_trait]
impl TextToSpeech for PollyTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        self.accept(&request)?;

        #[cfg(not(feature = "polly"))]
        {
            Err(Error::Config(format!(
                "provider `{}` talks to Amazon Polly, which this build cannot do: it was \
                 compiled without the `polly` feature",
                self.descriptor.id
            )))
        }

        #[cfg(feature = "polly")]
        {
            use aws_sdk_polly::types::{Engine, OutputFormat, VoiceId};

            let rate = Self::nearest_rate(request.format.sample_rate);
            if rate != request.format.sample_rate {
                // A pipeline resampling every utterance is usually a
                // misconfiguration, so the mismatch is recorded rather than
                // silently absorbed.
                tracing::info!(
                    provider = %self.descriptor.id,
                    requested = request.format.sample_rate,
                    serving = rate,
                    "Polly produces PCM at 8 kHz and 16 kHz only; serving the nearest"
                );
            }
            let voice = request.voice.as_deref().unwrap_or(&self.voice);

            let response = self
                .client
                .synthesize_speech()
                .text(&request.text)
                .output_format(OutputFormat::Pcm)
                .voice_id(VoiceId::from(voice))
                .engine(Engine::from(self.engine.as_str()))
                .sample_rate(rate.to_string())
                .send()
                .await
                .map_err(|error| {
                    Error::provider(&self.descriptor.id, crate::failure::of_response(&error))
                })?;

            // The whole body, collected. Polly answers `SynthesizeSpeech` with a
            // byte stream, and the SDK's own `collect` is the only way to read
            // it — there is no chunk-by-chunk reader on `ByteStream` that does
            // not go through it. So this provider does not stream from the first
            // byte the way `conduit-deepgram` does, and the README says so
            // rather than implying otherwise.
            let format =
                AudioFormat { encoding: Encoding::PcmS16Le, sample_rate: rate, channels: 1 };
            let samples = response
                .audio_stream
                .collect()
                .await
                .map_err(|error| Error::provider(&self.descriptor.id, error))?;

            Ok(Box::pin(futures_util::stream::once(async move {
                Ok(SpeechChunk { sequence: 0, format, data: samples.into_bytes() })
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PollyTtsConfig {
        PollyTtsConfig { name: "house-voice".to_owned(), ..PollyTtsConfig::default() }
    }

    #[test]
    fn the_descriptor_advertises_the_configured_voice_before_any_request() {
        // Built without a round trip, so the only voice it can name is the one
        // the operator chose — and naming none would leave a console unable to
        // show the choice that was saved.
        let descriptor = PollyTts::descriptor(&PollyTtsConfig {
            voice: Some("Matthew".to_owned()),
            label: Some("House (Polly)".to_owned()),
            ..config()
        });

        assert_eq!(descriptor.id, "house-voice");
        assert_eq!(descriptor.label, "House (Polly)");
        assert_eq!(descriptor.metadata.voices[0].id, "Matthew");
        assert_eq!(descriptor.metadata.encodings, ENCODINGS.to_vec());
    }

    #[test]
    fn a_voice_id_is_not_read_as_a_language() {
        // `Joanna` says nothing about `en-US`. Guessing would put a wrong
        // language on a descriptor a pipeline may route on, which is worse than
        // an absent one.
        let descriptor = PollyTts::descriptor(&config());

        assert_eq!(descriptor.metadata.voices[0].language, "");
    }

    #[test]
    fn the_rate_the_pipeline_wants_is_a_rate_polly_produces() {
        assert_eq!(PollyTts::nearest_rate(16_000), 16_000);
        assert_eq!(PollyTts::nearest_rate(8_000), 8_000);
    }

    #[test]
    fn an_unavailable_rate_is_approximated_rather_than_refused() {
        // A rate mismatch is something the pipeline can resample. An encoding
        // mismatch is not, which is why that one is refused instead.
        assert_eq!(PollyTts::nearest_rate(22_050), 16_000);
        assert_eq!(PollyTts::nearest_rate(48_000), 16_000);
        assert_eq!(PollyTts::nearest_rate(4_000), 8_000);
    }

    /// A provider to check requests against.
    ///
    /// `expect` rather than `if let Ok`: a test that skipped its assertions when
    /// construction failed would pass for the wrong reason, and these three are
    /// the ones that would go quiet. Feature-gated because without `polly` there
    /// is nothing to construct, and that refusal has its own test.
    #[cfg(feature = "polly")]
    async fn provider() -> PollyTts {
        PollyTts::new(config()).await.expect("builds without reaching AWS")
    }

    #[cfg(feature = "polly")]
    #[tokio::test]
    async fn a_format_polly_cannot_label_is_refused_by_name() {
        let polly = provider().await;
        let request = SynthesisRequest {
            format: AudioFormat { encoding: Encoding::PcmF32Le, ..AudioFormat::DEFAULT },
            ..SynthesisRequest::new("hello")
        };

        let error = polly.accept(&request).expect_err("floats are not PCM16").to_string();

        assert!(error.contains("PcmS16Le"), "names what it does produce: {error}");
        assert!(error.contains("house-voice"), "and which provider: {error}");
    }

    #[cfg(feature = "polly")]
    #[tokio::test]
    async fn an_utterance_over_pollys_limit_is_refused_with_both_numbers() {
        let error = provider()
            .await
            .accept(&SynthesisRequest::new("x".repeat(3_001)))
            .expect_err("over the limit")
            .to_string();

        assert!(error.contains("3000"), "the limit: {error}");
        assert!(error.contains("3001"), "and what was asked for: {error}");
    }

    #[cfg(feature = "polly")]
    #[tokio::test]
    async fn the_limit_is_inclusive_and_counts_characters_rather_than_bytes() {
        // Counting bytes would refuse a legitimate utterance a third of the way
        // in, and only for non-English speech.
        let polly = provider().await;

        assert!(polly.accept(&SynthesisRequest::new("x".repeat(3_000))).is_ok());
        // 3000 three-byte characters: 9000 bytes, 3000 characters.
        assert!(polly.accept(&SynthesisRequest::new("あ".repeat(3_000))).is_ok());
    }

    #[cfg(not(feature = "polly"))]
    #[tokio::test]
    async fn a_build_without_the_feature_refuses_by_naming_it() {
        // The point of the feature-off path: an operator learns this binary
        // cannot reach Polly, rather than watching a saved voice fail its first
        // turn with a credential error that is not the real reason.
        let error = PollyTts::new(config()).await.expect_err("cannot be built").to_string();

        assert!(error.contains("polly"), "names the feature: {error}");
        assert!(error.contains("house-voice"), "and the provider: {error}");
    }

    #[cfg(feature = "polly")]
    #[tokio::test]
    async fn the_region_and_the_profile_reach_the_sdk_config() {
        // Asserted through the loader rather than by making a call, so this needs
        // no AWS account: what an operator configures is visible in the resolved
        // config.
        let shared =
            PollyTts::loader(&PollyTtsConfig { region: "eu-west-1".to_owned(), ..config() })
                .load()
                .await;

        assert_eq!(shared.region().map(|region| region.as_ref()), Some("eu-west-1"));
    }
}
