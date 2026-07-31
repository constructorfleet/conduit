//! Speech recognition and synthesis against a stand-in server.

mod server;

use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_openai::{OpenAiConfig, OpenAiStt, OpenAiTts};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions};
use conduit_provider::tts::{SynthesisRequest, TextToSpeech};
use conduit_provider::ChunkStream;
use futures_util::StreamExt;
use server::MockServer;

fn config(server: &MockServer) -> OpenAiConfig {
    OpenAiConfig {
        base_url: server.url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiConfig::default()
    }
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

#[tokio::test]
async fn a_transcript_comes_back_as_one_final_result() {
    let server =
        MockServer::start(r#"{"text":"turn on the light","language":"en"}"#.to_owned()).await;
    let stt = OpenAiStt::new(&config(&server), "whisper-1").expect("builds");

    let transcripts: Vec<_> = stt
        .transcribe(utterance(&[b"aa", b"bb"]), TranscribeOptions::default())
        .await
        .expect("transcribes")
        .map(|item| item.expect("ok"))
        .collect()
        .await;

    assert_eq!(transcripts.len(), 1, "the endpoint has no partials to report");
    assert_eq!(transcripts[0].text, "turn on the light");
    assert_eq!(transcripts[0].language.as_deref(), Some("en"));
    assert!(transcripts[0].is_final);
}

#[tokio::test]
async fn the_whole_utterance_is_uploaded_as_a_wav_file() {
    let server = MockServer::start(r#"{"text":"hi"}"#.to_owned()).await;
    let stt = OpenAiStt::new(&config(&server), "whisper-1").expect("builds");

    let _ = stt
        .transcribe(utterance(&[b"one", b"two"]), TranscribeOptions::default())
        .await
        .expect("transcribes")
        .collect::<Vec<_>>()
        .await;

    let content_type = server.last_content_type().await.expect("a request");
    assert!(content_type.starts_with("multipart/form-data"), "{content_type}");

    let body = server.last_raw().await.expect("a body");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains(r#"name="model""#) && text.contains("whisper-1"), "{text}");
    assert!(text.contains(r#"filename="audio.wav""#), "{text}");

    // Every chunk must reach the recognizer, in order, inside one file. The
    // search starts at the header because `form-data` in the multipart
    // preamble also contains "data".
    let riff = body.windows(4).position(|window| window == b"RIFF").expect("a WAV header");
    assert_eq!(&body[riff + 8..riff + 12], b"WAVE");
    let data = riff
        + body[riff..].windows(4).position(|window| window == b"data").expect("a data chunk");
    assert_eq!(&body[data + 8..data + 14], b"onetwo");
}

#[tokio::test]
async fn a_language_hint_is_forwarded_when_given() {
    let server = MockServer::start(r#"{"text":"hola"}"#.to_owned()).await;
    let stt = OpenAiStt::new(&config(&server), "whisper-1").expect("builds");

    let options = TranscribeOptions { language: Some("es".to_owned()), ..Default::default() };
    let _ = stt
        .transcribe(utterance(&[b"x"]), options)
        .await
        .expect("transcribes")
        .collect::<Vec<_>>()
        .await;

    let text = String::from_utf8_lossy(&server.last_raw().await.expect("a body")).into_owned();
    assert!(text.contains(r#"name="language""#) && text.contains("es"), "{text}");
}

#[tokio::test]
async fn audio_the_endpoint_cannot_accept_is_refused_before_upload() {
    let server = MockServer::start(r#"{"text":"unused"}"#.to_owned()).await;
    let stt = OpenAiStt::new(&config(&server), "whisper-1").expect("builds");

    // Raw Opus frames are not a file; uploading them would be nonsense.
    let options = TranscribeOptions {
        format: AudioFormat { encoding: Encoding::Opus, ..AudioFormat::DEFAULT },
        ..Default::default()
    };
    assert!(stt.transcribe(utterance(&[b"x"]), options).await.is_err());
    assert!(server.last_raw().await.is_none(), "nothing should have been sent");
}

#[tokio::test]
async fn a_rejected_transcription_becomes_a_provider_error() {
    let server = MockServer::start_status(500, "model is loading").await;
    let stt = OpenAiStt::new(&config(&server), "whisper-1").expect("builds");

    let Err(error) = stt.transcribe(utterance(&[b"x"]), TranscribeOptions::default()).await
    else {
        panic!("a 500 must not be reported as success");
    };
    assert!(error.to_string().contains("500"), "{error}");
    assert!(error.is_retryable());
}

#[tokio::test]
async fn synthesized_audio_is_forwarded_as_it_arrives() {
    // Three packets from the server must not be collapsed into one chunk;
    // playback should start on the first.
    let server =
        MockServer::start_chunked(vec!["one".to_owned(), "two".to_owned(), "three".to_owned()])
            .await;
    let tts = OpenAiTts::new(&config(&server), "tts-1").expect("builds");

    let chunks: Vec<_> = tts
        .synthesize(SynthesisRequest::new("hello"))
        .await
        .expect("synthesizes")
        .map(|item| item.expect("ok"))
        .collect()
        .await;

    assert_eq!(chunks.len(), 3, "expected streamed audio");
    assert_eq!(chunks.iter().map(|chunk| chunk.sequence).collect::<Vec<_>>(), [0, 1, 2]);
    let audio: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.to_vec()).collect();
    assert_eq!(String::from_utf8(audio).expect("utf-8"), "onetwothree");
}

#[tokio::test]
async fn the_synthesis_request_names_the_model_voice_and_format() {
    let server = MockServer::start("audio".to_owned()).await;
    let tts = OpenAiTts::new(&config(&server), "tts-1").expect("builds");

    let request =
        SynthesisRequest { voice: Some("nova".to_owned()), ..SynthesisRequest::new("hello") };
    let _ = tts.synthesize(request).await.expect("synthesizes").collect::<Vec<_>>().await;

    let body = server.last_body().await.expect("a request");
    assert_eq!(body["model"], "tts-1");
    assert_eq!(body["input"], "hello");
    assert_eq!(body["voice"], "nova");
    assert_eq!(body["response_format"], "pcm");
}

#[tokio::test]
async fn a_pipeline_that_names_no_voice_gets_a_default() {
    let server = MockServer::start("audio".to_owned()).await;
    let tts = OpenAiTts::new(&config(&server), "tts-1").expect("builds");

    let _ = tts
        .synthesize(SynthesisRequest::new("hello"))
        .await
        .expect("synthesizes")
        .collect::<Vec<_>>()
        .await;

    let body = server.last_body().await.expect("a request");
    assert!(body["voice"].as_str().is_some_and(|voice| !voice.is_empty()));
}

#[tokio::test]
async fn a_format_the_endpoint_cannot_produce_is_refused() {
    let server = MockServer::start("audio".to_owned()).await;
    let tts = OpenAiTts::new(&config(&server), "tts-1").expect("builds");

    let request = SynthesisRequest {
        format: AudioFormat { encoding: Encoding::PcmF32Le, ..AudioFormat::DEFAULT },
        ..SynthesisRequest::new("hello")
    };
    assert!(tts.synthesize(request).await.is_err());
}

#[tokio::test]
async fn the_voice_catalogue_can_be_replaced_for_a_local_server() {
    let server = MockServer::start("audio".to_owned()).await;
    let tts = OpenAiTts::new(&config(&server), "tts-1").expect("builds").with_voices(vec![
        conduit_provider::tts::Voice {
            id: "en_GB-alan-medium".to_owned(),
            name: "Alan".to_owned(),
            language: "en-GB".to_owned(),
        },
    ]);

    let voices = tts.voices().await.expect("lists voices");
    assert_eq!(voices.len(), 1);
    assert_eq!(voices[0].id, "en_GB-alan-medium");
}

#[tokio::test]
async fn both_providers_send_the_api_key() {
    let server = MockServer::start(r#"{"text":"hi"}"#.to_owned()).await;
    let stt = OpenAiStt::new(&config(&server), "whisper-1").expect("builds");
    let _ = stt
        .transcribe(utterance(&[b"x"]), TranscribeOptions::default())
        .await
        .expect("transcribes")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(server.last_authorization().await.as_deref(), Some("Bearer test-key"));

    let tts = OpenAiTts::new(&config(&server), "tts-1").expect("builds");
    let _ = tts
        .synthesize(SynthesisRequest::new("hi"))
        .await
        .expect("synthesizes")
        .collect::<Vec<_>>()
        .await;
    assert_eq!(server.last_authorization().await.as_deref(), Some("Bearer test-key"));
}
