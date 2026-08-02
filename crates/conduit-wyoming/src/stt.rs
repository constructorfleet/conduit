//! Speech-to-text over the Wyoming protocol.
//!
//! Wyoming STT servers (e.g. faster-whisper) accept an `audio-start` event
//! describing the stream, followed by `audio-chunk` events carrying raw
//! samples and an `audio-stop` event. They answer with a `transcript` event
//! carrying the final text, preceded on a streaming server by any number of
//! `transcript-chunk` partials.

use conduit_core::audio::Encoding;
use conduit_core::{Error, Result};
use conduit_provider::stt::{AudioChunk, SpeechToText, TranscribeOptions, Transcript};
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::io::BufReader;
use tokio::net::TcpStream;

use crate::protocol::{
    read_wyoming_event, tcp_address, write_wyoming_event, write_wyoming_event_with_payload,
    WyomingEvent, CONNECT_TIMEOUT,
};

/// A speech-to-text provider backed by a Wyoming TCP server.
#[derive(Debug, Clone)]
pub struct WyomingStt {
    /// Stable registration name, surfaced in health and diagnostics.
    name: String,
    /// Resolved `host:port` from the `tcp://` URL.
    address: String,
    /// Optional server-side model to request in `audio-start`.
    model: Option<String>,
    /// Whether the server is expected to emit partial transcripts. Partials
    /// are still gated on the per-request `partials` option; this flag only
    /// describes the stored definition.
    #[allow(dead_code)]
    streaming: bool,
}

impl WyomingStt {
    /// Builds a provider for the server at `url`, which must be
    /// `tcp://host:port`. Does not connect.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `url` is not a `tcp://host:port` address.
    pub fn new(
        name: impl Into<String>,
        url: &str,
        model: Option<String>,
        streaming: bool,
    ) -> Result<Self> {
        let name = name.into();
        let address = tcp_address(url).ok_or_else(|| {
            Error::Config(format!("provider `{name}` Wyoming url must use tcp://host:port"))
        })?;
        Ok(Self { name, address, model, streaming })
    }

    async fn connect(&self) -> Result<TcpStream> {
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&self.address))
            .await
            .map_err(|_| {
                Error::Config(format!("provider `{}` timed out connecting", self.name))
            })?
            .map_err(|error| Error::provider(self.name.clone(), error))
    }
}

/// Event type carrying a partial transcript on a streaming server.
const TRANSCRIPT_CHUNK: &str = "transcript-chunk";

/// Splits a transcript event into text, confidence, and finality.
///
/// Finality comes from the *event type*, not the payload: a `transcript` event
/// is the final transcript — that is the whole of what `Transcript(text=...)`
/// serializes to — and only a streaming server's [`TRANSCRIPT_CHUNK`] is a
/// partial. A `result` object is not part of the protocol; it is read when
/// present, because a server that adds one has better text and a confidence to
/// offer, but requiring it meant never accepting a transcript at all.
fn transcript_from_event(event: &WyomingEvent) -> (String, Option<f32>, bool) {
    let is_final = event.event_type != TRANSCRIPT_CHUNK;
    let result = event.data.get("result").and_then(Value::as_object);
    let text = result
        .and_then(|result| result.get("text"))
        .and_then(Value::as_str)
        .or_else(|| event.data.get("text").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    let confidence = result
        .and_then(|result| result.get("confidence"))
        .and_then(Value::as_f64)
        .map(|confidence| confidence as f32);
    (text, confidence, is_final)
}

#[async_trait::async_trait]
impl Provider for WyomingStt {
    fn name(&self) -> &str {
        &self.name
    }

    async fn health(&self) -> Health {
        match self.connect().await {
            Ok(_) => Health::Healthy,
            Err(error) => Health::Unhealthy { reason: error.to_string() },
        }
    }
}

#[async_trait::async_trait]
impl SpeechToText for WyomingStt {
    async fn transcribe(
        &self,
        audio: ChunkStream<AudioChunk>,
        options: TranscribeOptions,
    ) -> Result<ChunkStream<Transcript>> {
        let stream = self.connect().await?;
        let (read_half, mut write_half) = stream.into_split();

        // Wyoming reads the format off every `audio-chunk`, not just off
        // `audio-start` — `AudioChunk.from_event` treats `rate`, `width` and
        // `channels` as required keys — so the same description is repeated on
        // each chunk below.
        let chunk_format = json!({
            "rate": options.format.sample_rate,
            "width": 2,
            "channels": options.format.channels,
        });
        let mut data = chunk_format.clone();
        data["encoding"] = json!("pcm_s16le");
        if let Some(model) = &self.model {
            data["model"] = json!(model);
        }
        write_wyoming_event(&mut write_half, "audio-start", data).await?;

        // Held by both the pump and the transcript stream, so the socket is
        // only closed once the caller is done with it.
        //
        // `audio-stop` already ends the audio in band. Dropping the write half
        // as soon as the pump finished said the same thing again as a FIN, and
        // a server built on asyncio tears the whole connection down when its
        // event loop sees EOF — whether or not the handler is reading. That
        // raced the transcription and destroyed transcripts the server had
        // already produced, which reached an operator as a turn that captured
        // clean audio and recognized nothing.
        let writer = std::sync::Arc::new(tokio::sync::Mutex::new(write_half));
        let pump_writer = std::sync::Arc::clone(&writer);
        let provider = self.name.clone();
        let pump_provider = provider.clone();
        tokio::spawn(async move {
            let mut guard = pump_writer.lock().await;
            let write_half = &mut *guard;
            let mut audio = audio;
            while let Some(chunk) = audio.next().await {
                match chunk {
                    Ok(chunk) => {
                        let sequence = chunk.sequence;
                        if let Err(error) = write_wyoming_event_with_payload(
                            write_half,
                            "audio-chunk",
                            chunk_format.clone(),
                            &chunk.data,
                        )
                        .await
                        {
                            tracing::warn!(
                                pump_provider,
                                sequence,
                                error = %error,
                                "failed to forward audio chunk; closing Wyoming session"
                            );
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            pump_provider,
                            error = %error,
                            "audio input failed; closing Wyoming session"
                        );
                        return;
                    }
                }
            }
            if let Err(error) = write_wyoming_event(write_half, "audio-stop", json!({})).await {
                tracing::warn!(pump_provider, error = %error, "failed to signal end of audio");
            }
        });

        let reader = BufReader::new(read_half);
        let emit_partials = options.partials;
        Ok(Box::pin(stream::unfold(
            (reader, provider, emit_partials, false, writer),
            |(mut reader, provider, emit_partials, mut saw_final, writer)| async move {
                loop {
                    match read_wyoming_event(&mut reader).await {
                        Ok(Some(event))
                            if event.event_type == "transcript"
                                || event.event_type == TRANSCRIPT_CHUNK =>
                        {
                            let (text, confidence, is_final) = transcript_from_event(&event);
                            if is_final {
                                saw_final = true;
                                let transcript = Transcript {
                                    text,
                                    is_final: true,
                                    confidence,
                                    language: None,
                                    start_ms: None,
                                };
                                return Some((
                                    Ok(transcript),
                                    (reader, provider, emit_partials, saw_final, writer),
                                ));
                            }
                            if emit_partials {
                                return Some((
                                    Ok(Transcript::partial(text)),
                                    (reader, provider, emit_partials, saw_final, writer),
                                ));
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            // A clean end of stream still owes us a final
                            // transcript; report it once, then finish.
                            if !saw_final {
                                saw_final = true;
                                return Some((
                                    Err(Error::provider(
                                        provider.clone(),
                                        std::io::Error::new(
                                            std::io::ErrorKind::UnexpectedEof,
                                            "connection closed before final transcript",
                                        ),
                                    )),
                                    (reader, provider, emit_partials, saw_final, writer),
                                ));
                            }
                            return None;
                        }
                        Err(error) => {
                            return Some((
                                Err(Error::provider(provider.clone(), error)),
                                (reader, provider, emit_partials, saw_final, writer),
                            ));
                        }
                    }
                }
            },
        )))
    }

    fn supports_encoding(&self, encoding: Encoding) -> bool {
        encoding == Encoding::PcmS16Le
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::audio::AudioFormat;
    use conduit_core::Error;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// Accepts one connection and returns every event the provider wrote.
    async fn collect_session_events(listener: TcpListener) -> Vec<WyomingEvent> {
        let (stream, _) = listener.accept().await.expect("provider connects");
        let mut reader = BufReader::new(stream);
        let mut events = Vec::new();
        while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
            let stop = event.event_type == "audio-stop";
            events.push(event);
            if stop {
                break;
            }
        }
        events
    }

    #[tokio::test]
    async fn the_write_half_stays_open_until_the_transcript_arrives() {
        // `audio-stop` already says the audio is over, in band. Dropping the
        // write half straight afterwards says it again as a FIN, and a server
        // built on asyncio tears the connection down when it reads EOF —
        // losing a transcript it had already produced. This models that server:
        // it treats EOF as the end of the connection, so it only answers if we
        // are still holding the socket open.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("provider connects");
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut saw_stop = false;
            loop {
                match read_wyoming_event(&mut reader).await {
                    Ok(Some(event)) if event.event_type == "audio-stop" => {
                        saw_stop = true;
                        break;
                    }
                    Ok(Some(_)) => {}
                    // EOF: an asyncio server stops here and the socket dies.
                    Ok(None) | Err(_) => break,
                }
            }
            if !saw_stop {
                return Err("connection ended before audio-stop".to_owned());
            }
            // asyncio's event loop notices the peer's FIN whether or not the
            // handler is reading, and tears the connection down. Racing the
            // transcription against a further read is what models that; a
            // server that only looked at EOF when it chose to read would not.
            let mut byte = [0_u8; 1];
            tokio::select! {
                _ = reader.read(&mut byte) => {
                    return Err("connection torn down on EOF before the transcript".to_owned());
                }
                () = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
            write_wyoming_event(
                &mut write_half,
                "transcript",
                json!({ "text": "what time is it" }),
            )
            .await
            .map_err(|error| error.to_string())
        });

        let provider = WyomingStt::new("whisper", &format!("tcp://{address}"), None, false)
            .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let mut transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");

        let first = transcripts.next().await.expect("a transcript").expect("not an error");
        assert!(first.is_final);
        assert_eq!(first.text, "what time is it");
        server.await.expect("server task").expect("the transcript must reach the client");
    }

    #[tokio::test]
    async fn a_transcript_sent_after_audio_stop_still_arrives() {
        // faster-whisper transcribes only once it sees `audio-stop`, so the
        // final transcript always lands *after* the provider has finished
        // writing and dropped its write half. If that drop tears down the whole
        // socket rather than half-closing it, the server's write is reset and
        // the turn reports "connection closed before final transcript" for a
        // transcript that was successfully produced.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("provider connects");
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
                if event.event_type == "audio-stop" {
                    break;
                }
            }
            // Stand in for transcription time.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            write_wyoming_event(&mut write_half, "transcript", json!({ "text": "never mind" }))
                .await
        });

        let provider = WyomingStt::new("whisper", &format!("tcp://{address}"), None, false)
            .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let mut transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");

        let first = transcripts.next().await.expect("a transcript").expect("not an error");
        assert!(first.is_final);
        assert_eq!(first.text, "never mind");
        server.await.expect("server task").expect("the server's write must not be reset");
    }

    #[tokio::test]
    async fn every_audio_chunk_describes_its_own_format() {
        // Wyoming's `AudioChunk.from_event` reads `rate`, `width` and
        // `channels` off each chunk, not off `audio-start`. Sending them only
        // once makes faster-whisper raise `KeyError` on the first chunk and
        // drop the session, which reaches an operator as a turn that captured
        // audio and transcribed nothing.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(collect_session_events(listener));

        let provider = WyomingStt::new("whisper", &format!("tcp://{address}"), None, false)
            .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let _transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");

        let events = server.await.expect("server task");
        let chunk = events
            .iter()
            .find(|event| event.event_type == "audio-chunk")
            .expect("a chunk was sent");
        assert_eq!(chunk.data.get("rate").and_then(Value::as_u64), Some(16000));
        assert_eq!(chunk.data.get("width").and_then(Value::as_u64), Some(2));
        assert_eq!(chunk.data.get("channels").and_then(Value::as_u64), Some(1));
        assert_eq!(chunk.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn new_accepts_a_tcp_url() {
        let provider =
            WyomingStt::new("whisper", "tcp://localhost:10300", Some("small".to_owned()), true)
                .unwrap();
        assert_eq!(provider.name(), "whisper");
        assert_eq!(provider.address, "localhost:10300");
        assert_eq!(provider.model.as_deref(), Some("small"));
        assert!(provider.streaming);
    }

    #[test]
    fn new_accepts_a_model_less_url() {
        let provider =
            WyomingStt::new("whisper", "tcp://localhost:10300", None, false).unwrap();
        assert!(provider.model.is_none());
        assert!(!provider.streaming);
    }

    #[test]
    fn new_rejects_non_tcp_urls() {
        let error =
            WyomingStt::new("whisper", "ws://localhost:10300", None, false).unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn new_rejects_malformed_tcp_urls() {
        let error = WyomingStt::new("whisper", "tcp://noport", None, false).unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn supports_only_pcm_s16_le() {
        let provider =
            WyomingStt::new("whisper", "tcp://localhost:10300", None, false).unwrap();
        assert!(provider.supports_encoding(Encoding::PcmS16Le));
        assert!(!provider.supports_encoding(Encoding::Flac));
    }

    #[test]
    fn transcript_event_with_result_is_final() {
        let event = WyomingEvent {
            event_type: "transcript".to_owned(),
            data: json!({
                "text": "partial text",
                "result": { "text": "final text", "confidence": 0.95 }
            }),
            payload: Vec::new(),
        };
        let (text, confidence, is_final) = transcript_from_event(&event);
        assert_eq!(text, "final text");
        assert_eq!(confidence, Some(0.95));
        assert!(is_final);
    }

    #[test]
    fn a_bare_transcript_event_is_final() {
        // What faster-whisper actually sends: `Transcript(text=...)`, whose
        // data is just `{"text": ...}`. Treating the absence of a `result`
        // object as "this is only a partial" left every real transcript
        // discarded and the turn waiting until the idle timeout fired.
        let event = WyomingEvent {
            event_type: "transcript".to_owned(),
            data: json!({ "text": "what time is it" }),
            payload: Vec::new(),
        };
        let (text, confidence, is_final) = transcript_from_event(&event);
        assert_eq!(text, "what time is it");
        assert_eq!(confidence, None);
        assert!(is_final, "a `transcript` event is the final transcript");
    }

    #[test]
    fn a_transcript_chunk_is_a_partial() {
        // Streaming servers send partials under their own event type; only
        // that type is non-final.
        let event = WyomingEvent {
            event_type: TRANSCRIPT_CHUNK.to_owned(),
            data: json!({ "text": "what time" }),
            payload: Vec::new(),
        };
        let (text, _, is_final) = transcript_from_event(&event);
        assert_eq!(text, "what time");
        assert!(!is_final);
    }
}
