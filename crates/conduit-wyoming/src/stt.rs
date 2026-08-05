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
use conduit_provider::{Capability, ChunkStream, Descriptor, Health, Metadata, Provider};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::io::BufReader;
use tokio::net::TcpStream;

use crate::protocol::{
    advertises_streaming, error_message, read_wyoming_event, tcp_address, write_wyoming_event,
    write_wyoming_event_with_payload, WyomingEvent, WyomingServerError, CONNECT_TIMEOUT,
    DESCRIBE, ERROR, INFO,
};

/// The one encoding Wyoming audio events carry.
const ENCODINGS: [Encoding; 1] = [Encoding::PcmS16Le];

/// A speech-to-text provider backed by a Wyoming TCP server.
#[derive(Debug, Clone)]
pub struct WyomingStt {
    /// Identity, version, and what this server says it can do.
    descriptor: Descriptor,
    /// Resolved `host:port` from the `tcp://` URL.
    address: String,
    /// Optional server-side model to request in `audio-start`.
    model: Option<String>,
    /// Whether this definition asks for partial transcripts.
    ///
    /// `false` means none are emitted, whatever the server offers. `true` means
    /// the server is asked first — see [`Self::server_streams`] — and a server
    /// that cannot stream still produces a correct single final, because a
    /// non-streaming recognizer is a fully working recognizer.
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
        let descriptor = Descriptor::new(name, Capability::Stt).with_metadata(
            Metadata::default()
                .with_models(model.iter().cloned().collect())
                .with_encodings(ENCODINGS.to_vec()),
        );
        Ok(Self { descriptor, address, model, streaming })
    }

    /// Sets the human-readable name operator screens show.
    ///
    /// Separate from the identity this provider was built with: the identity
    /// is what a pipeline selects and what appears in metric labels, and this
    /// is only what a person reads.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.with_label(label);
        self
    }

    async fn connect(&self) -> Result<TcpStream> {
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&self.address))
            .await
            .map_err(|_| {
                Error::Config(format!("provider `{}` timed out connecting", self.name()))
            })?
            .map_err(|error| Error::provider(self.name().to_owned(), error))
    }

    /// Asks the server whether it can stream partial transcripts.
    ///
    /// `describe` is sent on its own short-lived connection rather than on the
    /// session socket. Wyoming allows the handshake in band, but a server is
    /// free to answer `info` at any point, and reading it off the session would
    /// mean the transcript loop had to tell a late `info` apart from the one it
    /// asked for. A second connection costs a TCP setup against a recognizer
    /// that is about to do far more expensive work.
    ///
    /// Never fails a turn. Every failure — a refused connection, a timeout, a
    /// server that answers something else — returns `None`, meaning "no answer",
    /// and the caller proceeds. A recognizer that transcribes correctly must not
    /// be taken out of service because it would not introduce itself.
    async fn server_streams(&self) -> Option<bool> {
        let stream = self.connect().await.ok()?;
        let (read_half, mut write_half) = stream.into_split();
        write_wyoming_event(&mut write_half, DESCRIBE, json!({})).await.ok()?;
        let mut reader = BufReader::new(read_half);
        // Bounded: a server that accepts the connection and then says nothing
        // would otherwise hold the turn open before any audio was sent.
        let deadline = tokio::time::timeout(CONNECT_TIMEOUT, async {
            loop {
                match read_wyoming_event(&mut reader).await {
                    Ok(Some(event)) if event.event_type == INFO => {
                        return advertises_streaming(&event, "asr")
                    }
                    // Anything else on the way to `info` is not ours to act on.
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => return None,
                }
            }
        });
        deadline.await.ok().flatten()
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
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
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
        // What the definition asks for, and what the server will actually do.
        //
        // The stored flag decides whether partials are wanted at all: a
        // definition with `streaming` off emits none even from a server that
        // offers them, which is the case that was impossible while the request
        // option was the only gate. `options.partials` still has a veto, so a
        // caller that cannot use partials does not receive them.
        //
        // Only when partials are wanted is the server asked, because a server
        // that will not be sending any is not worth a round trip.
        let emit_partials = if self.streaming && options.partials {
            match self.server_streams().await {
                Some(false) => {
                    // Once per session, at info, naming the server and the
                    // reason. This is the fallback, and it is not a failure: a
                    // non-streaming recognizer is a fully working recognizer.
                    tracing::info!(
                        provider = self.name(),
                        address = %self.address,
                        "server does not support transcript streaming; \
                         falling back to a single final transcript"
                    );
                    false
                }
                // A server that says yes, and a server that did not answer.
                // Silence is not a refusal — the capability key postdates
                // transcript streaming, and `describe` may have failed for
                // reasons that have nothing to do with recognition — so
                // partials stay on and an absent one costs nothing.
                Some(true) | None => true,
            }
        } else {
            false
        };

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
        let provider = self.name().to_owned();
        let pump_provider = provider.clone();
        // How much audio reached the recognizer, and how the send side ended,
        // is the difference between "the device stopped talking" and "we
        // stopped forwarding" — and neither is visible from the server's logs,
        // which only ever see what arrived.
        let started = std::time::Instant::now();
        tokio::spawn(async move {
            let mut guard = pump_writer.lock().await;
            let write_half = &mut *guard;
            let mut audio = audio;
            let mut chunks = 0_u64;
            let mut bytes = 0_usize;
            while let Some(chunk) = audio.next().await {
                match chunk {
                    Ok(chunk) => {
                        let sequence = chunk.sequence;
                        let len = chunk.data.len();
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
                                chunks,
                                bytes,
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                error = %error,
                                "failed to forward audio chunk; closing Wyoming session"
                            );
                            return;
                        }
                        chunks += 1;
                        bytes += len;
                    }
                    Err(error) => {
                        tracing::warn!(
                            pump_provider,
                            chunks,
                            bytes,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            error = %error,
                            "audio input failed; closing Wyoming session"
                        );
                        return;
                    }
                }
            }
            match write_wyoming_event(write_half, "audio-stop", json!({})).await {
                Ok(()) => tracing::info!(
                    pump_provider,
                    chunks,
                    bytes,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "sent audio-stop; waiting for the final transcript"
                ),
                Err(error) => tracing::warn!(
                    pump_provider,
                    chunks,
                    bytes,
                    error = %error,
                    "failed to signal end of audio"
                ),
            }
        });

        let reader = BufReader::new(read_half);
        Ok(Box::pin(stream::unfold(
            (reader, provider, emit_partials, false, writer),
            |(mut reader, provider, emit_partials, mut saw_final, writer)| async move {
                // The final transcript ends the session: it is the answer the
                // turn was waiting for, and there is nothing further to read.
                // Waiting for the server to hang up instead only worked while
                // the write half was dropped early enough to make it hang up —
                // a server that keeps the connection open for another utterance
                // never sends EOF, and the stage timed out sixty seconds after
                // the transcript it had already read.
                if saw_final {
                    return None;
                }
                loop {
                    match read_wyoming_event(&mut reader).await {
                        Ok(Some(event))
                            if event.event_type == "transcript"
                                || event.event_type == TRANSCRIPT_CHUNK =>
                        {
                            let (text, confidence, is_final) = transcript_from_event(&event);
                            // At info because this is the one fact that
                            // distinguishes "the server never answered" from
                            // "the server answered and we did not accept it",
                            // and the two look identical from outside. One
                            // line per turn.
                            tracing::info!(
                                provider,
                                event = %event.event_type,
                                is_final,
                                chars = text.len(),
                                "received a Wyoming transcript"
                            );
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
                        // A refusal the server took the trouble to explain.
                        // Reported as the session's failure, because the server
                        // closes next and the EOF branch below would otherwise
                        // report a closed connection — which reads as a network
                        // fault for what is usually a format mismatch the
                        // operator can fix in the console.
                        //
                        // `saw_final` is checked first so this cannot undo a
                        // turn that already succeeded: a server free to send an
                        // error after answering would otherwise fail a
                        // transcript the caller has already been handed. In
                        // practice the stream has ended by then; the guard is
                        // for the server that does it anyway.
                        Ok(Some(event)) if event.event_type == ERROR => {
                            let message = error_message(&event);
                            if saw_final {
                                tracing::warn!(
                                    provider,
                                    message = %message,
                                    "Wyoming server reported an error after the final transcript; \
                                     keeping the transcript"
                                );
                                return None;
                            }
                            tracing::warn!(
                                provider,
                                message = %message,
                                "Wyoming server refused the transcription"
                            );
                            // Ends the session: the server has said no and will
                            // close. Set before returning so a subsequent poll
                            // stops rather than reading the EOF as a second,
                            // less accurate failure.
                            saw_final = true;
                            return Some((
                                Err(Error::provider(
                                    provider.clone(),
                                    WyomingServerError::new(message),
                                )),
                                (reader, provider, emit_partials, saw_final, writer),
                            ));
                        }
                        Ok(Some(event)) => {
                            tracing::debug!(
                                provider,
                                event = %event.event_type,
                                "ignoring a Wyoming event that is not a transcript"
                            );
                        }
                        Ok(None) => {
                            // A clean end of stream still owes us a final
                            // transcript; report it once, then finish.
                            if !saw_final {
                                // A clean EOF here means the server closed
                                // without answering. Recording it separately
                                // from the error the turn reports says whether
                                // the close arrived before the transcript was
                                // written or after it was lost.
                                tracing::warn!(
                                    provider,
                                    "Wyoming server closed the connection with no final transcript"
                                );
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
    async fn the_stream_ends_once_the_final_transcript_is_delivered() {
        // A server that keeps the connection open for another utterance never
        // sends EOF, so ending the stream only on EOF left the turn waiting on
        // a session that had already answered — the transcription stage timed
        // out sixty seconds after a transcript it had successfully read.
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
            write_wyoming_event(&mut write_half, "transcript", json!({ "text": "that's it" }))
                .await
                .expect("transcript written");
            // Deliberately holds the socket open, as faster-whisper does.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
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
        assert_eq!(first.text, "that's it");

        let ended = tokio::time::timeout(std::time::Duration::from_secs(2), transcripts.next())
            .await
            .expect("the stream must end without waiting for the server to hang up");
        assert!(ended.is_none(), "a final transcript ends the session");
        server.abort();
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
        let metadata = &provider.descriptor().metadata;
        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(!metadata.supports_encoding(Encoding::Flac));
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
            event_type: "transcript-chunk".to_owned(),
            data: json!({ "text": "what time" }),
            payload: Vec::new(),
        };
        let (text, _, is_final) = transcript_from_event(&event);
        assert_eq!(text, "what time");
        assert!(!is_final);
    }

    /// How a fake server answers `describe`.
    #[derive(Clone, Copy)]
    enum Describes {
        /// Answers `info` advertising transcript streaming.
        Streaming,
        /// Answers `info` advertising that it cannot stream, as a live
        /// faster-whisper server does.
        NotStreaming,
        /// Answers `info` with no capability key at all, as a server predating
        /// the flag does.
        Silent,
    }

    impl Describes {
        fn info(self) -> Option<Value> {
            match self {
                Self::Streaming => Some(
                    json!({ "asr": [{ "name": "fake", "supports_transcript_streaming": true }] }),
                ),
                Self::NotStreaming => Some(
                    json!({ "asr": [{ "name": "fake", "supports_transcript_streaming": false }] }),
                ),
                Self::Silent => Some(json!({ "asr": [{ "name": "fake" }] })),
            }
        }
    }

    /// Runs a fake recognizer that answers `describe`, then sends `chunks` as
    /// partials followed by `final_text`, and returns everything the caller saw.
    ///
    /// Handles connections in a loop because negotiation and the session are
    /// separate connections, and answers whichever the client asks for — so a
    /// client that skips `describe` entirely is served correctly too, which is
    /// what makes "no round trip when partials are off" testable.
    async fn transcripts_from(
        describes: Describes,
        provider_streaming: bool,
        request_partials: bool,
        chunks: &[&str],
        final_text: &str,
    ) -> Vec<Transcript> {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let owned: Vec<String> = chunks.iter().map(|chunk| (*chunk).to_owned()).collect();
        let final_text = final_text.to_owned();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("provider connects");
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut described = false;
                while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
                    if event.event_type == DESCRIBE {
                        if let Some(info) = describes.info() {
                            write_wyoming_event(&mut write_half, INFO, info)
                                .await
                                .expect("info written");
                        }
                        described = true;
                        break;
                    }
                    if event.event_type == "audio-stop" {
                        for chunk in &owned {
                            write_wyoming_event(
                                &mut write_half,
                                TRANSCRIPT_CHUNK,
                                json!({ "text": chunk }),
                            )
                            .await
                            .expect("partial written");
                        }
                        write_wyoming_event(
                            &mut write_half,
                            "transcript",
                            json!({ "text": final_text }),
                        )
                        .await
                        .expect("final written");
                        return;
                    }
                }
                if !described {
                    return;
                }
            }
        });

        let provider =
            WyomingStt::new("whisper", &format!("tcp://{address}"), None, provider_streaming)
                .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options = TranscribeOptions {
            format: AudioFormat::DEFAULT,
            partials: request_partials,
            ..TranscribeOptions::default()
        };
        let transcripts = provider.transcribe(Box::pin(audio), options).await.expect("session");
        let collected: Vec<Transcript> =
            transcripts.map(|item| item.expect("no error")).collect().await;
        server.abort();
        collected
    }

    #[tokio::test]
    async fn a_streaming_server_yields_partials_before_the_final() {
        // The whole point of the flag: a server that says it can stream is asked
        // to, and the caller sees the text arrive as it is recognized.
        let transcripts = transcripts_from(
            Describes::Streaming,
            true,
            true,
            &["the quick", "the quick brown"],
            "the quick brown fox",
        )
        .await;

        let partials: Vec<&str> =
            transcripts.iter().filter(|t| !t.is_final).map(|t| t.text.as_str()).collect();
        assert_eq!(partials, ["the quick", "the quick brown"]);
        let last = transcripts.last().expect("a final");
        assert!(last.is_final);
        assert_eq!(last.text, "the quick brown fox");
    }

    #[tokio::test]
    async fn streaming_against_a_server_that_cannot_stream_still_transcribes() {
        // The fallback, and the criterion that matters most. A live
        // faster-whisper server answers `describe` with
        // `supports_transcript_streaming: False`; asking it for partials must
        // not fail the turn, because a non-streaming recognizer is a fully
        // working recognizer. It sends a `transcript-chunk` anyway here — a
        // server contradicting its own handshake must not produce partials the
        // negotiation said would not come.
        let transcripts = transcripts_from(
            Describes::NotStreaming,
            true,
            true,
            &["ignored"],
            "the whole of it",
        )
        .await;

        assert_eq!(transcripts.len(), 1, "only the final, got: {transcripts:?}");
        assert!(transcripts[0].is_final);
        assert_eq!(transcripts[0].text, "the whole of it");
    }

    #[tokio::test]
    async fn streaming_off_yields_no_partials_even_when_the_server_sends_them() {
        // Impossible before this change: `TranscribeOptions` defaulted
        // `partials: true` and the stored flag was never read, so nobody could
        // turn partials off. The server sends two, and neither is emitted.
        let transcripts = transcripts_from(
            Describes::Streaming,
            false,
            true,
            &["half a", "half a sentence"],
            "a whole sentence",
        )
        .await;

        assert_eq!(transcripts.len(), 1, "only the final, got: {transcripts:?}");
        assert!(transcripts[0].is_final);
        assert_eq!(transcripts[0].text, "a whole sentence");
    }

    #[tokio::test]
    async fn a_caller_that_cannot_use_partials_keeps_its_veto() {
        // The request option still wins. A definition asking for streaming does
        // not force partials on a caller that has nowhere to put them.
        let transcripts =
            transcripts_from(Describes::Streaming, true, false, &["partial"], "final").await;

        assert_eq!(transcripts.len(), 1, "only the final, got: {transcripts:?}");
        assert!(transcripts[0].is_final);
    }

    #[tokio::test]
    async fn a_server_that_does_not_say_gets_the_benefit_of_the_doubt() {
        // `supports_transcript_streaming` postdates transcript streaming
        // itself, so an older server that does stream omits the key. Reading
        // silence as a refusal would turn partials off against servers that
        // support them.
        let transcripts =
            transcripts_from(Describes::Silent, true, true, &["a partial"], "a final").await;

        let partials: Vec<&str> =
            transcripts.iter().filter(|t| !t.is_final).map(|t| t.text.as_str()).collect();
        assert_eq!(partials, ["a partial"], "silence is not a refusal");
    }

    #[tokio::test]
    async fn a_server_that_will_not_describe_itself_still_transcribes() {
        // Negotiation must never fail a turn. This server accepts the
        // `describe` connection and says nothing at all, so the handshake times
        // out — and the session still has to produce its transcript.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("provider connects");
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut is_session = false;
                while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
                    if event.event_type == "audio-stop" {
                        is_session = true;
                        break;
                    }
                }
                if is_session {
                    write_wyoming_event(
                        &mut write_half,
                        "transcript",
                        json!({ "text": "heard" }),
                    )
                    .await
                    .expect("final written");
                    return;
                }
                // The describe connection answered nothing and the client has
                // now given up on it. Loop straight round to accept the session
                // rather than holding the socket: sleeping here would make the
                // test wait for the sleep, not for the handshake timeout this
                // is about.
            }
        });

        let provider =
            WyomingStt::new("whisper", &format!("tcp://{address}"), None, true).expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let mut transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");

        let transcript = transcripts.next().await.expect("a transcript").expect("not an error");
        assert_eq!(transcript.text, "heard");
        assert!(transcript.is_final);
        server.abort();
    }

    #[tokio::test]
    async fn negotiation_is_skipped_when_no_partials_are_wanted() {
        // A server that will not be sending partials is not worth a round trip.
        // The fake refuses to answer `describe` at all — if the client waited
        // for an answer it would stall for the handshake timeout, so this also
        // proves the skip rather than just tolerating it.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let seen_describe = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server_flag = std::sync::Arc::clone(&seen_describe);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("provider connects");
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
                if event.event_type == DESCRIBE {
                    server_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                if event.event_type == "audio-stop" {
                    break;
                }
            }
            write_wyoming_event(&mut write_half, "transcript", json!({ "text": "batch only" }))
                .await
                .expect("final written");
        });

        let provider = WyomingStt::new("whisper", &format!("tcp://{address}"), None, false)
            .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let mut transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");

        let transcript = transcripts.next().await.expect("a transcript").expect("not an error");
        assert_eq!(transcript.text, "batch only");
        assert!(
            !seen_describe.load(std::sync::atomic::Ordering::SeqCst),
            "a definition with streaming off must not ask the server anything"
        );
        server.abort();
    }

    /// Drives a real session against a server that refuses, and returns the
    /// error the caller sees.
    ///
    /// The server sends `error` and then closes, which is what a Wyoming
    /// server does on a format it will not accept. Closing matters: it is the
    /// EOF branch that used to win the race to report a failure.
    async fn refused_session_error(refusal: Value) -> Error {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("provider connects");
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            // Refuses on `audio-start`, before any audio, as a server checking
            // the declared format does.
            let _ = read_wyoming_event(&mut reader).await;
            write_wyoming_event(&mut write_half, "error", refusal)
                .await
                .expect("error written");
            drop(write_half);
        });

        let provider = WyomingStt::new("whisper", &format!("tcp://{address}"), None, false)
            .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let mut transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");
        let error = transcripts
            .next()
            .await
            .expect("the refusal must be reported, not swallowed")
            .expect_err("a refused session is not a transcript");
        server.abort();
        error
    }

    #[tokio::test]
    async fn a_server_refusal_reaches_the_operator_instead_of_a_closed_connection() {
        // The symptom this fixes: a server refusing a sample-rate mismatch sent
        // a message naming what arrived and what it wanted, Conduit had no arm
        // for `error`, and the operator was told the connection closed — which
        // reads as a network fault for a configuration mistake.
        let error = refused_session_error(
            json!({ "text": "sample rate 16000 is not supported, expected 48000" }),
        )
        .await;

        let message = error.to_string();
        assert!(
            message.contains("sample rate 16000 is not supported, expected 48000"),
            "the server's own message must reach the operator, got: {message}"
        );
        assert!(
            !message.contains("connection closed before final transcript"),
            "the EOF message must not stand in for a refusal the server explained, got: {message}"
        );
        assert!(
            matches!(&error, Error::Provider { provider, .. } if provider == "whisper"),
            "a refusal is a provider failure naming the provider, got: {error:?}"
        );
    }

    #[tokio::test]
    async fn a_refusal_carrying_a_code_reports_the_code_too() {
        // Some servers send `code` alongside `text`, and some send only one of
        // the two. Whatever arrives has to end up in front of the operator.
        let error = refused_session_error(
            json!({ "text": "unsupported format", "code": "bad_request" }),
        )
        .await;

        let message = error.to_string();
        assert!(message.contains("unsupported format"), "got: {message}");
        assert!(message.contains("bad_request"), "got: {message}");
    }

    #[tokio::test]
    async fn a_refusal_with_no_message_still_says_a_refusal_arrived() {
        // Both fields are optional in practice. Reporting nothing would hand
        // the caller straight back to the closed-connection story.
        let error = refused_session_error(json!({})).await;

        let message = error.to_string();
        assert!(
            !message.contains("connection closed before final transcript"),
            "an empty refusal is still a refusal, got: {message}"
        );
        assert!(message.contains("error"), "got: {message}");
    }

    #[tokio::test]
    async fn an_error_after_the_final_transcript_does_not_fail_the_turn() {
        // Which wins, decided: the transcript. The caller has already been
        // handed it, and a turn that produced correct text is a turn that
        // succeeded — retracting it because the server spoke again afterwards
        // would fail a session on something the operator cannot act on.
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
            write_wyoming_event(
                &mut write_half,
                "transcript",
                json!({ "text": "the whole of it" }),
            )
            .await
            .expect("transcript written");
            write_wyoming_event(
                &mut write_half,
                "error",
                json!({ "text": "too late to matter" }),
            )
            .await
            .expect("error written");
            drop(write_half);
        });

        let provider = WyomingStt::new("whisper", &format!("tcp://{address}"), None, false)
            .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let mut transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");

        let transcript = transcripts.next().await.expect("a transcript").expect("not an error");
        assert_eq!(transcript.text, "the whole of it");
        assert!(transcript.is_final);
        assert!(
            transcripts.next().await.is_none(),
            "a late error must not be reported after a turn that already answered"
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_close_with_no_error_and_no_transcript_keeps_its_own_message() {
        // This change narrows when the EOF message appears; it does not replace
        // it. A server that closes without explaining itself genuinely is a
        // closed connection, and saying so is still the most accurate thing.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("provider connects");
            // Reads the session out before closing. Dropping the socket the
            // instant it is accepted resets the connection instead of ending it,
            // which is a different failure than the one this covers.
            let (read_half, write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
                if event.event_type == "audio-stop" {
                    break;
                }
            }
            drop(write_half);
        });

        let provider = WyomingStt::new("whisper", &format!("tcp://{address}"), None, false)
            .expect("built");
        let audio =
            stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]);
        let options =
            TranscribeOptions { format: AudioFormat::DEFAULT, ..TranscribeOptions::default() };
        let mut transcripts =
            provider.transcribe(Box::pin(audio), options).await.expect("session");

        let error = transcripts
            .next()
            .await
            .expect("a silent close is still a failure")
            .expect_err("no transcript arrived");
        assert!(
            error.to_string().contains("connection closed before final transcript"),
            "got: {error}"
        );
        server.abort();
    }
}
