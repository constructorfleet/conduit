//! Synthesis and batch transcription against a stand-in server.

mod server;

use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_elevenlabs::{ElevenLabsConfig, ElevenLabsStt, ElevenLabsTts};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions};
use conduit_provider::tts::{SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Provider};
use futures_util::StreamExt;
use server::MockServer;

/// A documented voice id, so the traversal check is exercised on the way past
/// rather than bypassed.
const VOICE: &str = "21m00Tcm4TlvDq8ikWAM";

fn config(server: &MockServer) -> ElevenLabsConfig {
    ElevenLabsConfig {
        base_url: server.url(),
        api_key: Some("sk_test".to_owned()),
        voice_id: Some(VOICE.to_owned()),
        ..ElevenLabsConfig::default()
    }
}

fn synthesizer(server: &MockServer) -> ElevenLabsTts {
    ElevenLabsTts::new(&config(server)).expect("builds")
}

fn recognizer(server: &MockServer) -> ElevenLabsStt {
    ElevenLabsStt::new(&config(server)).expect("builds")
}

/// An utterance arriving in several chunks, as a device would send it.
fn utterance(chunks: &[&[u8]]) -> ChunkStream<AudioChunk> {
    let chunks: Vec<_> = chunks
        .iter()
        .enumerate()
        .map(|(sequence, data)| {
            Ok(AudioChunk { sequence: sequence as u64, data: Bytes::copy_from_slice(data) })
        })
        .collect();
    Box::pin(futures_util::stream::iter(chunks))
}

/// Runs a synthesis to completion and returns the chunks.
async fn speak(
    tts: &ElevenLabsTts,
    request: SynthesisRequest,
) -> Vec<conduit_provider::tts::SpeechChunk> {
    tts.synthesize(request)
        .await
        .expect("synthesizes")
        .map(|item| item.expect("ok"))
        .collect()
        .await
}

// ---------------------------------------------------------------- synthesis

#[tokio::test]
async fn synthesized_audio_is_forwarded_as_it_arrives() {
    // Three packets from the server must not be collapsed into one chunk;
    // playback should start on the first.
    let server = MockServer::start_chunked(&["one", "two", "three"]).await;
    let tts = synthesizer(&server);

    let chunks = speak(&tts, SynthesisRequest::new("hello")).await;

    assert_eq!(chunks.len(), 3, "expected streamed audio");
    assert_eq!(chunks.iter().map(|chunk| chunk.sequence).collect::<Vec<_>>(), [0, 1, 2]);
    let audio: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.to_vec()).collect();
    assert_eq!(String::from_utf8(audio).expect("utf-8"), "onetwothree");
}

#[tokio::test]
async fn the_voice_is_a_path_segment_and_the_format_is_a_query_parameter() {
    // The two ways this vendor differs from every other provider here, and the
    // two things most likely to be wrong.
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    let _ = speak(&tts, SynthesisRequest::new("hello")).await;

    assert_eq!(
        server.last_path().await.as_deref(),
        Some("/v1/text-to-speech/21m00Tcm4TlvDq8ikWAM/stream")
    );
    assert_eq!(server.last_voice_id().await.as_deref(), Some(VOICE));
    assert_eq!(server.last_output_format().await.as_deref(), Some("pcm_16000"));
}

#[tokio::test]
async fn the_request_body_names_the_text_and_the_model() {
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    let _ = speak(&tts, SynthesisRequest::new("turn on the light")).await;

    let body = server.last_body().await.expect("a request");
    assert_eq!(body["text"], "turn on the light");
    assert_eq!(body["model_id"], conduit_elevenlabs::DEFAULT_TTS_MODEL);
    assert!(body.get("voice_id").is_none(), "the voice is in the path, not the body: {body}");
}

#[tokio::test]
async fn a_requested_voice_replaces_the_configured_one_in_the_path() {
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    let request = SynthesisRequest {
        voice: Some("9BWtsMINqrJLrRacOk9x".to_owned()),
        ..SynthesisRequest::new("hello")
    };
    let _ = speak(&tts, request).await;

    assert_eq!(server.last_voice_id().await.as_deref(), Some("9BWtsMINqrJLrRacOk9x"));
}

#[tokio::test]
async fn voice_controls_travel_in_the_body_and_the_rate_becomes_speed() {
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    let descriptor = tts.descriptor().clone();
    let settings = descriptor
        .validate_settings(&serde_json::json!({ "stability": 0.3, "use_speaker_boost": true }))
        .expect("declared settings");
    let request =
        SynthesisRequest { rate: Some(1.1), settings, ..SynthesisRequest::new("hello") };
    let _ = speak(&tts, request).await;

    let body = server.last_body().await.expect("a request");
    assert_eq!(body["voice_settings"]["stability"], 0.3);
    assert_eq!(body["voice_settings"]["use_speaker_boost"], true);
    assert_eq!(body["voice_settings"]["speed"], 1.1, "the request's rate, under its wire name");
}

#[tokio::test]
async fn a_request_that_configures_nothing_sends_no_voice_settings_at_all() {
    // The vendor's defaults are per-voice, so sending `stability: 0.5` because
    // nothing was configured would overwrite the operator's own tuning.
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    let _ = speak(&tts, SynthesisRequest::new("hello")).await;

    let body = server.last_body().await.expect("a request");
    assert!(body.get("voice_settings").is_none(), "{body}");
}

#[tokio::test]
async fn chunks_report_the_format_that_was_actually_produced() {
    // The documented contract: a provider that cannot honour the requested
    // format says what it did produce, on the first chunk, rather than
    // mislabelling the audio.
    let server = MockServer::start_chunked(&["a", "b"]).await;
    let tts = synthesizer(&server);

    let requested = AudioFormat { sample_rate: 96_000, channels: 2, ..AudioFormat::DEFAULT };
    let chunks =
        speak(&tts, SynthesisRequest { format: requested, ..SynthesisRequest::new("hi") })
            .await;

    let produced =
        AudioFormat { encoding: Encoding::PcmS16Le, sample_rate: 48_000, channels: 1 };
    assert!(chunks.iter().all(|chunk| chunk.format == produced), "{chunks:?}");
    assert_ne!(chunks[0].format, requested, "the honest answer, not the requested one");
    assert_eq!(
        server.last_output_format().await.as_deref(),
        Some("pcm_48000"),
        "and it asked for the nearest rate the vendor offers"
    );
}

#[tokio::test]
async fn the_interchange_format_is_requested_as_pcm_needing_no_transcode() {
    let server = MockServer::start_chunked(&["a"]).await;
    let tts = synthesizer(&server);

    let chunks = speak(&tts, SynthesisRequest::new("hi")).await;

    assert_eq!(chunks[0].format, AudioFormat::DEFAULT, "no transcode is needed");
    assert_eq!(server.last_output_format().await.as_deref(), Some("pcm_16000"));
}

#[tokio::test]
async fn a_format_the_endpoint_cannot_produce_is_refused_before_anything_is_sent() {
    let server = MockServer::start("audio").await;
    let tts = synthesizer(&server);

    for encoding in [Encoding::PcmF32Le, Encoding::Opus, Encoding::Flac] {
        let request = SynthesisRequest {
            format: AudioFormat { encoding, ..AudioFormat::DEFAULT },
            ..SynthesisRequest::new("hello")
        };
        assert!(tts.synthesize(request).await.is_err(), "{encoding:?} must be refused");
    }
    assert_eq!(server.synthesis_calls(), 0, "nothing should have been sent");
}

// ------------------------------------------------------------- transcription

#[tokio::test]
async fn a_transcript_comes_back_as_one_final_result() {
    let server = MockServer::start(
        r#"{"language_code":"en","language_probability":0.98,"text":"turn on the light",
            "words":[{"text":"turn","start":0,"end":0.2}]}"#,
    )
    .await;
    let stt = recognizer(&server);

    let transcripts: Vec<_> = stt
        .transcribe(utterance(&[b"aa", b"bb"]), TranscribeOptions::default())
        .await
        .expect("transcribes")
        .map(|item| item.expect("ok"))
        .collect()
        .await;

    assert_eq!(transcripts.len(), 1, "the batch endpoint has no partials to report");
    assert_eq!(transcripts[0].text, "turn on the light");
    assert_eq!(transcripts[0].language.as_deref(), Some("en"));
    assert!(transcripts[0].is_final);
}

#[tokio::test]
async fn a_language_probability_is_not_reported_as_a_transcript_confidence() {
    // They are different numbers. A caller thresholding on `confidence` would
    // otherwise be thresholding on how sure the model is about the *language*.
    let server =
        MockServer::start(r#"{"text":"hi","language_code":"en","language_probability":0.4}"#)
            .await;
    let stt = recognizer(&server);

    let transcripts: Vec<_> = stt
        .transcribe(utterance(&[b"x"]), TranscribeOptions::default())
        .await
        .expect("transcribes")
        .map(|item| item.expect("ok"))
        .collect()
        .await;

    assert_eq!(transcripts[0].confidence, None, "a language probability is not a confidence");
    assert_eq!(transcripts[0].language.as_deref(), Some("en"));
}

#[tokio::test]
async fn the_whole_utterance_is_uploaded_as_one_wav_file_naming_the_model() {
    let server = MockServer::start(r#"{"text":"hi"}"#).await;
    let stt = recognizer(&server);

    let _ = stt
        .transcribe(utterance(&[b"one", b"two"]), TranscribeOptions::default())
        .await
        .expect("transcribes")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(server.last_path().await.as_deref(), Some("/v1/speech-to-text"));
    let content_type = server.last_content_type().await.expect("a request");
    assert!(content_type.starts_with("multipart/form-data"), "{content_type}");

    let body = server.last_raw().await.expect("a body");
    let text = String::from_utf8_lossy(&body);
    // `model_id`, not `model`: a wrong field name here is silently ignored.
    assert!(text.contains(r#"name="model_id""#), "{text}");
    assert!(text.contains(conduit_elevenlabs::DEFAULT_STT_MODEL), "{text}");
    assert!(text.contains(r#"filename="audio.wav""#), "{text}");

    // Every chunk must reach the recognizer, in order, inside one file. The
    // search starts at the header because `form-data` in the multipart preamble
    // also contains "data".
    let riff = body.windows(4).position(|window| window == b"RIFF").expect("a WAV header");
    assert_eq!(&body[riff + 8..riff + 12], b"WAVE");
    let data = riff
        + body[riff..].windows(4).position(|window| window == b"data").expect("data chunk");
    assert_eq!(&body[data + 8..data + 14], b"onetwo");
}

#[tokio::test]
async fn a_language_hint_travels_as_language_code() {
    let server = MockServer::start(r#"{"text":"hola"}"#).await;
    let stt = recognizer(&server);

    let options = TranscribeOptions { language: Some("es".to_owned()), ..Default::default() };
    let _ = stt
        .transcribe(utterance(&[b"x"]), options)
        .await
        .expect("transcribes")
        .collect::<Vec<_>>()
        .await;

    let text = String::from_utf8_lossy(&server.last_raw().await.expect("a body")).into_owned();
    assert!(text.contains(r#"name="language_code""#), "not `language`: {text}");
    assert!(text.contains("es"), "{text}");
}

#[tokio::test]
async fn declared_settings_are_forwarded_as_form_fields() {
    let server = MockServer::start(r#"{"text":"hi"}"#).await;
    let stt = recognizer(&server);

    let descriptor = stt.descriptor().clone();
    let settings = descriptor
        .validate_settings(&serde_json::json!({ "diarize": true, "num_speakers": 2 }))
        .expect("declared settings");
    let options = TranscribeOptions { settings, ..Default::default() };
    let _ = stt
        .transcribe(utterance(&[b"x"]), options)
        .await
        .expect("transcribes")
        .collect::<Vec<_>>()
        .await;

    let text = String::from_utf8_lossy(&server.last_raw().await.expect("a body")).into_owned();
    assert!(text.contains(r#"name="diarize""#) && text.contains("true"), "{text}");
    assert!(text.contains(r#"name="num_speakers""#) && text.contains('2'), "{text}");
}

#[tokio::test]
async fn audio_that_is_not_a_file_is_refused_before_upload() {
    let server = MockServer::start(r#"{"text":"unused"}"#).await;
    let stt = recognizer(&server);

    // Raw Opus frames are not a file; uploading them would be nonsense.
    let options = TranscribeOptions {
        format: AudioFormat { encoding: Encoding::Opus, ..AudioFormat::DEFAULT },
        ..Default::default()
    };
    assert!(stt.transcribe(utterance(&[b"x"]), options).await.is_err());
    assert!(server.last_raw().await.is_none(), "nothing should have been sent");
}

// ----------------------------------------------------------------- catalogue

#[tokio::test]
async fn the_voice_catalogue_can_be_read_from_the_account() {
    let server = MockServer::start(
        r#"{"voices":[
            {"voice_id":"21m00Tcm4TlvDq8ikWAM","name":"Rachel",
             "verified_languages":[{"language":"en","model_id":"eleven_flash_v2_5"}]},
            {"voice_id":"9BWtsMINqrJLrRacOk9x","name":"Aria",
             "fine_tuning":{"language":"de"}}]}"#,
    )
    .await;
    let mut tts = synthesizer(&server);

    let voices = tts.load_voices().await.expect("reads the catalogue").to_vec();

    assert_eq!(server.last_path().await.as_deref(), Some("/v1/voices"));
    assert_eq!(voices.len(), 2);
    assert_eq!(voices[0].id, "21m00Tcm4TlvDq8ikWAM");
    assert_eq!(voices[0].name, "Rachel");
    assert_eq!(voices[0].language, "en");
    assert_eq!(voices[1].language, "de", "a fine-tuned language outranks a verified one");
    assert_eq!(tts.descriptor().metadata.voices, voices, "and it is advertised");
}

#[tokio::test]
async fn a_catalogue_voice_that_could_not_be_a_path_is_dropped_rather_than_offered() {
    // The account's own catalogue is not a trusted input: a cloned voice's id
    // comes from whatever created it. Offering one that would redirect the
    // request is how the traversal check would get bypassed in practice.
    let server = MockServer::start(
        r#"{"voices":[{"voice_id":"../../v1/user","name":"Sneaky"},
                      {"voice_id":"21m00Tcm4TlvDq8ikWAM","name":"Rachel"}]}"#,
    )
    .await;
    let mut tts = synthesizer(&server);

    let voices = tts.load_voices().await.expect("reads the catalogue").to_vec();

    assert_eq!(voices.len(), 1, "only the usable voice is offered");
    assert_eq!(voices[0].id, "21m00Tcm4TlvDq8ikWAM");
}

#[tokio::test]
async fn a_provider_with_no_voice_configured_refuses_rather_than_guessing_one() {
    // Voice ids are account-scoped, so there is no `alloy` to fall back to.
    let server = MockServer::start("audio").await;
    let tts = ElevenLabsTts::new(&ElevenLabsConfig {
        base_url: server.url(),
        voice_id: None,
        ..ElevenLabsConfig::default()
    })
    .expect("builds");

    let Err(error) = tts.synthesize(SynthesisRequest::new("hello")).await else {
        panic!("a provider with no voice must refuse");
    };
    assert!(error.to_string().contains("voice_id"), "{error}");
    assert_eq!(server.synthesis_calls(), 0, "nothing should have been sent");
}

#[tokio::test]
async fn a_configured_catalogue_supplies_the_voice_when_a_request_names_none() {
    let server = MockServer::start("audio").await;
    let tts = ElevenLabsTts::new(&ElevenLabsConfig {
        base_url: server.url(),
        voice_id: None,
        voices: vec![Voice {
            id: "9BWtsMINqrJLrRacOk9x".to_owned(),
            name: "Aria".to_owned(),
            language: "en".to_owned(),
        }],
        ..ElevenLabsConfig::default()
    })
    .expect("builds");

    let _ = speak(&tts, SynthesisRequest::new("hello")).await;

    assert_eq!(server.last_voice_id().await.as_deref(), Some("9BWtsMINqrJLrRacOk9x"));
}

// ---------------------------------------------------------------- credential

#[tokio::test]
async fn both_capabilities_send_the_key_as_xi_api_key_and_not_as_a_bearer_token() {
    // A bearer token is accepted by the transport and rejected by the API, so
    // getting this wrong produces a 401 that says nothing about the header.
    let server = MockServer::start(r#"{"text":"hi"}"#).await;

    let stt = recognizer(&server);
    let _ = stt
        .transcribe(utterance(&[b"x"]), TranscribeOptions::default())
        .await
        .expect("transcribes")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(server.last_api_key().await.as_deref(), Some("sk_test"));
    assert_eq!(server.last_authorization().await, None, "not a bearer token");

    let tts = synthesizer(&server);
    let _ = speak(&tts, SynthesisRequest::new("hi")).await;
    assert_eq!(server.last_api_key().await.as_deref(), Some("sk_test"));
    assert_eq!(server.last_authorization().await, None, "not a bearer token");
}

#[tokio::test]
async fn both_capabilities_report_health_from_a_call_that_needs_the_credential() {
    // A health check that skipped the key would report a rejected key as
    // healthy, which is worse than not checking at all.
    let server = MockServer::start(r#"{"voices":[]}"#).await;

    assert_eq!(synthesizer(&server).health().await, conduit_provider::Health::Healthy);
    assert_eq!(server.last_api_key().await.as_deref(), Some("sk_test"));

    assert_eq!(recognizer(&server).health().await, conduit_provider::Health::Healthy);
    assert_eq!(server.last_api_key().await.as_deref(), Some("sk_test"));
}
