//! The JSON and query shapes of the ElevenLabs API.
//!
//! Kept separate from the providers so the mapping between Conduit's vocabulary
//! and the vendor's is in one place and readable on its own.

use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// Sample rates the synthesis endpoint offers as PCM.
///
/// The `output_format` values are `codec_sample_rate_bitrate`, so PCM — which
/// has no bitrate — is `pcm_<rate>`. Ascending, because
/// [`nearest_pcm_rate`] scans them in order.
const PCM_RATES: [u32; 7] = [8_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000];

/// What the synthesis endpoint will be asked to produce, and what that audio
/// actually is.
///
/// The two halves exist separately because they can disagree: a request naming
/// a rate the vendor does not offer still gets audio, just not at the rate that
/// was asked for. [`SynthesisRequest::format`] documents that a provider which
/// cannot honour the requested format reports what it produced on the first
/// chunk, so this type carries the honest answer alongside the query value.
///
/// [`SynthesisRequest::format`]: conduit_provider::tts::SynthesisRequest::format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    /// The `output_format` query value, e.g. `pcm_16000`.
    pub query: PcmRate,
    /// What the returned bytes are, which is what chunks are labelled with.
    pub produced: AudioFormat,
}

/// One of the PCM rates the endpoint offers.
///
/// A newtype rather than a bare `u32` so the `output_format` string can only be
/// built from a rate that was checked against [`PCM_RATES`] — the query value
/// reaches a URL, and "some number the caller had" is not a thing to format
/// into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmRate(u32);

impl PcmRate {
    /// The `output_format` query value for this rate.
    #[must_use]
    pub fn as_query(self) -> String {
        format!("pcm_{}", self.0)
    }

    /// The rate itself.
    #[must_use]
    pub const fn hz(self) -> u32 {
        self.0
    }
}

impl OutputFormat {
    /// What to ask for, given what the pipeline wants.
    ///
    /// Always PCM, never MP3, and that is a decision rather than an oversight.
    /// [`Encoding`] has no MP3 variant, so a chunk of MP3 could only be
    /// labelled as something it is not — and a mislabelled chunk is worse than
    /// a refused request, because it plays back as noise several stages later
    /// with nothing pointing here. PCM is also what the pipeline's interchange
    /// format already is, so the common case needs no transcode at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] naming the encoding for anything the endpoint
    /// does not produce as PCM: 32-bit float, Opus, and FLAC.
    pub fn for_request(requested: AudioFormat) -> Result<Self> {
        match requested.encoding {
            Encoding::PcmS16Le => {}
            Encoding::PcmF32Le => {
                return Err(Error::Config(
                    "ElevenLabs does not produce 32-bit float PCM; ask for PcmS16Le".to_owned(),
                ))
            }
            // The endpoint does offer `opus_48000_*`, and it is left unused on
            // purpose: the documentation does not say whether those bytes are
            // Ogg-encapsulated or raw frames, and `Encoding::Opus` means raw
            // frames here. Guessing wrong produces audio that decodes to
            // silence, so this refuses rather than guesses.
            Encoding::Opus => {
                return Err(Error::Config(
                    "ElevenLabs Opus output is not supported: its framing is unconfirmed, and \
                     mislabelled Opus decodes to silence. Ask for PcmS16Le"
                        .to_owned(),
                ))
            }
            Encoding::Flac => {
                return Err(Error::Config(
                    "ElevenLabs does not produce FLAC; ask for PcmS16Le".to_owned(),
                ))
            }
            other => {
                return Err(Error::Config(format!("ElevenLabs does not produce {other:?}")))
            }
        }

        let rate = nearest_pcm_rate(requested.sample_rate);
        Ok(Self {
            query: rate,
            // Mono and 16-bit regardless of what was asked: that is what the
            // endpoint returns, and saying otherwise would be the mislabelling
            // this type exists to avoid.
            produced: AudioFormat {
                encoding: Encoding::PcmS16Le,
                sample_rate: rate.hz(),
                channels: 1,
            },
        })
    }

    /// Whether the audio is exactly what the request asked for.
    ///
    /// `false` is not a failure — it is the case the first chunk's format
    /// exists to report — but it is worth a log line, because a pipeline
    /// resampling every utterance is usually a misconfiguration.
    #[must_use]
    pub fn honours(&self, requested: AudioFormat) -> bool {
        self.produced == requested
    }
}

/// The offered PCM rate closest to `requested`.
///
/// Nearest rather than "the interchange rate for anything unusual": a request
/// for 48 kHz that quietly became 16 kHz would lose bandwidth no later stage
/// can recover, whereas the nearest offered rate keeps as much of the intent as
/// the vendor allows. Ties go to the lower rate, which is the cheaper of two
/// equally distant answers.
fn nearest_pcm_rate(requested: u32) -> PcmRate {
    let nearest = PCM_RATES
        .into_iter()
        .min_by_key(|rate| rate.abs_diff(requested))
        // `PCM_RATES` is a non-empty literal, so this is unreachable.
        .unwrap_or(AudioFormat::DEFAULT.sample_rate);
    PcmRate(nearest)
}

/// A synthesis request in the vendor's shape.
///
/// The voice is *not* here: it is a path segment, and it goes through
/// [`crate::voice_id::validate`] before it becomes one.
#[derive(Debug, Serialize)]
pub struct Synthesis {
    /// The text to speak.
    pub text: String,
    /// Which model speaks it, e.g. `eleven_flash_v2_5`.
    pub model_id: String,
    /// How the voice is rendered.
    #[serde(skip_serializing_if = "VoiceSettings::is_empty")]
    pub voice_settings: VoiceSettings,
    /// BCP-47 hint, where the model takes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_code: Option<String>,
}

/// The per-request voice controls.
///
/// Every field is optional and omitted when unset, because the vendor's
/// defaults are per-voice: sending `stability: 0.5` because nothing else was
/// configured would overwrite whatever the operator tuned on the voice itself.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct VoiceSettings {
    /// How consistent the delivery is, `0.0..=1.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stability: Option<f64>,
    /// How closely the output tracks the original voice, `0.0..=1.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_boost: Option<f64>,
    /// Style exaggeration, `0.0..=1.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<f64>,
    /// Whether to boost similarity to the speaker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_speaker_boost: Option<bool>,
    /// Speaking rate multiplier, where `1.0` is the voice's natural rate.
    ///
    /// This is where [`SynthesisRequest::rate`] lands. It is deliberately not
    /// also a declared setting: two ways to say the same thing means one of
    /// them silently loses.
    ///
    /// [`SynthesisRequest::rate`]: conduit_provider::tts::SynthesisRequest::rate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

impl VoiceSettings {
    /// Whether nothing was configured, so the object should be omitted rather
    /// than sent empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The transcription endpoint's answer for a single-channel upload.
///
/// Only the fields Conduit has somewhere to put. `words`, `entities`, and the
/// timing granularities are read and dropped: [`Transcript`] carries one text
/// and one optional offset, so word-level detail would have nowhere to go.
///
/// [`Transcript`]: conduit_provider::stt::Transcript
#[derive(Debug, Deserialize)]
pub struct Transcription {
    /// The recognized text.
    pub text: String,
    /// Detected language, when the model reports one.
    #[serde(default)]
    pub language_code: Option<String>,
    /// How confident the model is in the language it detected, `0.0..=1.0`.
    ///
    /// This is a *language* probability, not a transcription confidence, which
    /// is why it does not become [`Transcript::confidence`].
    ///
    /// [`Transcript::confidence`]: conduit_provider::stt::Transcript::confidence
    #[serde(default)]
    pub language_probability: Option<f32>,
}

/// The voice catalogue's answer.
#[derive(Debug, Deserialize)]
pub struct Catalogue {
    /// Voices on the account, premade and cloned alike.
    #[serde(default)]
    pub voices: Vec<CatalogueVoice>,
}

/// One voice as the catalogue describes it.
#[derive(Debug, Deserialize)]
pub struct CatalogueVoice {
    /// The id used in the synthesis path.
    pub voice_id: String,
    /// Display name, e.g. `"Rachel"`.
    #[serde(default)]
    pub name: Option<String>,
    /// Fine-tuning state, which is where a cloned voice records its language.
    #[serde(default)]
    pub fine_tuning: Option<FineTuning>,
    /// Languages the voice has been verified in.
    #[serde(default)]
    pub verified_languages: Vec<VerifiedLanguage>,
}

impl CatalogueVoice {
    /// This voice as Conduit describes one, or `None` if its id is not a value
    /// this crate will put in a URL.
    ///
    /// A catalogue entry is refused on the same terms as a configured voice.
    /// The account's own catalogue is not a trusted input: a cloned voice's id
    /// is chosen by whatever created it, and a name that cannot be a path
    /// segment must not become one just because it arrived over HTTPS.
    #[must_use]
    pub fn to_voice(&self) -> Option<conduit_provider::tts::Voice> {
        let id = crate::voice_id::validate(&self.voice_id)
            .inspect_err(|error| {
                tracing::warn!(%error, "skipping a catalogue voice with an unusable id");
            })
            .ok()?
            .to_owned();
        Some(conduit_provider::tts::Voice {
            name: self.name.clone().unwrap_or_else(|| id.clone()),
            language: self.language(),
            id,
        })
    }

    /// The language to advertise this voice as speaking.
    ///
    /// A fine-tuned voice records the language it was tuned for; a premade one
    /// does not, and lists the languages it has been verified in instead. When
    /// neither says anything the answer is [`crate::DEFAULT_LANGUAGE`], because
    /// [`Voice::language`] is not optional and an empty tag reads as a bug.
    ///
    /// [`Voice::language`]: conduit_provider::tts::Voice::language
    fn language(&self) -> String {
        self.fine_tuning
            .as_ref()
            .and_then(|tuning| tuning.language.clone())
            .or_else(|| self.verified_languages.first().map(|entry| entry.language.clone()))
            .unwrap_or_else(|| crate::DEFAULT_LANGUAGE.to_owned())
    }
}

/// The fine-tuning half of a catalogue entry.
#[derive(Debug, Deserialize)]
pub struct FineTuning {
    /// Language the voice was tuned for.
    #[serde(default)]
    pub language: Option<String>,
}

/// One language a voice has been verified in.
#[derive(Debug, Deserialize)]
pub struct VerifiedLanguage {
    /// BCP-47 or ISO-639 tag, as the vendor reports it.
    pub language: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interchange_format_is_asked_for_verbatim() {
        // The whole point of preferring PCM: the pipeline's own format needs no
        // transcode, so the common case is exact.
        let format = OutputFormat::for_request(AudioFormat::DEFAULT).expect("supported");
        assert_eq!(format.query.as_query(), "pcm_16000");
        assert_eq!(format.produced, AudioFormat::DEFAULT);
        assert!(format.honours(AudioFormat::DEFAULT));
    }

    #[test]
    fn mp3_is_never_requested_at_any_rate() {
        // `Encoding` has no MP3 variant, so an MP3 chunk could only be
        // mislabelled. Every offered rate must therefore come back as PCM.
        for sample_rate in [8_000, 16_000, 22_050, 24_000, 44_100, 48_000, 96_000] {
            let requested = AudioFormat { sample_rate, ..AudioFormat::DEFAULT };
            let format = OutputFormat::for_request(requested).expect("supported");
            let query = format.query.as_query();
            assert!(query.starts_with("pcm_"), "asked for `{query}`");
            assert_eq!(format.produced.encoding, Encoding::PcmS16Le);
        }
    }

    #[test]
    fn every_offered_rate_is_honoured_exactly() {
        for sample_rate in PCM_RATES {
            let requested =
                AudioFormat { sample_rate, channels: 1, encoding: Encoding::PcmS16Le };
            let format = OutputFormat::for_request(requested).expect("supported");
            assert_eq!(format.query.as_query(), format!("pcm_{sample_rate}"));
            assert!(format.honours(requested), "{sample_rate} Hz is offered verbatim");
        }
    }

    #[test]
    fn a_rate_the_vendor_does_not_offer_is_reported_rather_than_claimed() {
        // The contract a `SpeechChunk`'s format carries: a provider that could
        // not honour the request says what it actually produced.
        let requested = AudioFormat { sample_rate: 96_000, ..AudioFormat::DEFAULT };
        let format = OutputFormat::for_request(requested).expect("supported");

        assert_eq!(format.query.as_query(), "pcm_48000", "the nearest offered rate");
        assert_eq!(format.produced.sample_rate, 48_000);
        assert!(!format.honours(requested), "the difference must be visible to the caller");
    }

    #[test]
    fn a_multi_channel_request_is_reported_as_the_mono_it_receives() {
        let requested = AudioFormat { channels: 2, ..AudioFormat::DEFAULT };
        let format = OutputFormat::for_request(requested).expect("supported");

        assert_eq!(format.produced.channels, 1, "the endpoint returns mono");
        assert!(!format.honours(requested));
    }

    #[test]
    fn nearest_rate_prefers_the_lower_of_two_equally_distant_offers() {
        // 19 025 is 3 025 from both 16 000 and 22 050. A tie has to resolve the
        // same way every time or the same request would produce audio at two
        // different rates on two different days.
        assert_eq!(nearest_pcm_rate(19_025).hz(), 16_000);
        // And nearest genuinely means nearest, not "round down": 20 000 is
        // closer to 22 050 than to 16 000.
        assert_eq!(nearest_pcm_rate(20_000).hz(), 22_050);
        assert_eq!(nearest_pcm_rate(0).hz(), 8_000);
        assert_eq!(nearest_pcm_rate(u32::MAX).hz(), 48_000);
    }

    #[test]
    fn encodings_the_endpoint_cannot_produce_are_refused_with_an_alternative() {
        for encoding in [Encoding::PcmF32Le, Encoding::Opus, Encoding::Flac] {
            let requested = AudioFormat { encoding, ..AudioFormat::DEFAULT };
            let error = OutputFormat::for_request(requested).expect_err("unsupported");
            assert!(
                error.to_string().contains("PcmS16Le"),
                "the message must say what to ask for instead: {error}"
            );
        }
    }

    #[test]
    fn unset_voice_settings_are_omitted_rather_than_sent_as_the_vendors_defaults() {
        // Sending `stability: 0.5` because nothing was configured would
        // overwrite whatever the operator tuned on the voice itself.
        let body = Synthesis {
            text: "hello".to_owned(),
            model_id: "eleven_flash_v2_5".to_owned(),
            voice_settings: VoiceSettings::default(),
            language_code: None,
        };
        let json = serde_json::to_value(&body).expect("serializes");

        assert_eq!(json["text"], "hello");
        assert_eq!(json["model_id"], "eleven_flash_v2_5");
        assert!(json.get("voice_settings").is_none(), "{json}");
        assert!(json.get("language_code").is_none(), "{json}");
    }

    #[test]
    fn configured_voice_settings_travel_under_their_vendor_names() {
        let body = Synthesis {
            text: "hello".to_owned(),
            model_id: "eleven_flash_v2_5".to_owned(),
            voice_settings: VoiceSettings {
                stability: Some(0.3),
                similarity_boost: Some(0.9),
                style: Some(0.1),
                use_speaker_boost: Some(false),
                speed: Some(1.2),
            },
            language_code: Some("en".to_owned()),
        };
        let json = serde_json::to_value(&body).expect("serializes");

        assert_eq!(json["voice_settings"]["stability"], 0.3);
        assert_eq!(json["voice_settings"]["similarity_boost"], 0.9);
        assert_eq!(json["voice_settings"]["style"], 0.1);
        assert_eq!(json["voice_settings"]["use_speaker_boost"], false);
        assert_eq!(json["voice_settings"]["speed"], 1.2);
        assert_eq!(json["language_code"], "en");
    }

    #[test]
    fn a_transcription_is_read_from_the_documented_field_names() {
        let body: Transcription = serde_json::from_str(
            r#"{"language_code":"en","language_probability":0.98,"text":"Hello world!",
                "words":[{"text":"Hello","start":0,"end":0.5}],"transcription_id":"abc",
                "audio_duration_secs":10.5}"#,
        )
        .expect("the documented shape");

        assert_eq!(body.text, "Hello world!");
        assert_eq!(body.language_code.as_deref(), Some("en"));
        assert_eq!(body.language_probability, Some(0.98));
    }

    #[test]
    fn a_transcription_without_a_detected_language_still_reads() {
        let body: Transcription =
            serde_json::from_str(r#"{"text":"hi"}"#).expect("the minimal shape");
        assert_eq!(body.language_code, None);
        assert_eq!(body.language_probability, None);
    }

    #[test]
    fn a_catalogue_voice_becomes_a_conduit_voice() {
        let catalogue: Catalogue = serde_json::from_str(
            r#"{"voices":[{"voice_id":"21m00Tcm4TlvDq8ikWAM","name":"Rachel",
                "verified_languages":[{"language":"en","model_id":"eleven_flash_v2_5"}]}]}"#,
        )
        .expect("the documented shape");

        let voice = catalogue.voices[0].to_voice().expect("a usable voice");
        assert_eq!(voice.id, "21m00Tcm4TlvDq8ikWAM");
        assert_eq!(voice.name, "Rachel");
        assert_eq!(voice.language, "en");
    }

    #[test]
    fn a_fine_tuned_language_outranks_a_verified_one() {
        let voice: CatalogueVoice = serde_json::from_str(
            r#"{"voice_id":"abc123","fine_tuning":{"language":"de"},
                "verified_languages":[{"language":"en","model_id":"m"}]}"#,
        )
        .expect("the documented shape");

        assert_eq!(voice.to_voice().expect("usable").language, "de");
    }

    #[test]
    fn a_voice_that_names_no_language_falls_back_rather_than_advertising_an_empty_tag() {
        let voice: CatalogueVoice =
            serde_json::from_str(r#"{"voice_id":"abc123"}"#).expect("the minimal shape");

        let voice = voice.to_voice().expect("usable");
        assert_eq!(voice.language, crate::DEFAULT_LANGUAGE);
        assert_eq!(voice.name, "abc123", "an unnamed voice is labelled with its id");
    }

    #[test]
    fn a_catalogue_entry_whose_id_is_a_path_is_dropped_rather_than_offered() {
        // The account's catalogue is not a trusted input. A cloned voice's id
        // comes from whatever created it, and offering an operator a voice that
        // would redirect the request is how the traversal check gets bypassed.
        let catalogue: Catalogue = serde_json::from_str(
            r#"{"voices":[{"voice_id":"../../user","name":"Sneaky"},
                {"voice_id":"21m00Tcm4TlvDq8ikWAM","name":"Rachel"}]}"#,
        )
        .expect("the documented shape");

        let voices: Vec<_> =
            catalogue.voices.iter().filter_map(CatalogueVoice::to_voice).collect();
        assert_eq!(voices.len(), 1, "only the usable voice survives");
        assert_eq!(voices[0].id, "21m00Tcm4TlvDq8ikWAM");
    }
}
