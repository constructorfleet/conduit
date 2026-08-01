//! Speech-to-text over the Wyoming protocol.
//!
//! Wyoming STT servers (e.g. faster-whisper) accept an `audio-start` event
//! describing the stream, followed by `audio-chunk` events carrying raw
//! samples and an `audio-stop` event. They answer with `transcript` events —
//! partials first, then a final whose `data` carries a `result` object.

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

/// Splits a `transcript` event into text, confidence, and finality.
///
/// A transcript is final when the event data carries a `result` object; its
/// text comes from `result.text` (falling back to the top-level `text`) and
/// its confidence from `result.confidence`. Anything else is a partial.
fn transcript_from_event(event: &WyomingEvent) -> (String, Option<f32>, bool) {
    match event.data.get("result").and_then(Value::as_object) {
        Some(result) => {
            let text = result
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| event.data.get("text").and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned();
            let confidence = result.get("confidence").and_then(Value::as_f64).map(|c| c as f32);
            (text, confidence, true)
        }
        None => {
            let text =
                event.data.get("text").and_then(Value::as_str).unwrap_or_default().to_owned();
            (text, None, false)
        }
    }
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

        // Pump captured audio into the session. The server sees EOF when this
        // task drops the write half, which is exactly the behaviour we want
        // when the input stream itself fails.
        let provider = self.name.clone();
        let pump_provider = provider.clone();
        tokio::spawn(async move {
            let mut audio = audio;
            while let Some(chunk) = audio.next().await {
                match chunk {
                    Ok(chunk) => {
                        let sequence = chunk.sequence;
                        if let Err(error) = write_wyoming_event_with_payload(
                            &mut write_half,
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
            if let Err(error) =
                write_wyoming_event(&mut write_half, "audio-stop", json!({})).await
            {
                tracing::warn!(pump_provider, error = %error, "failed to signal end of audio");
            }
        });

        let reader = BufReader::new(read_half);
        let emit_partials = options.partials;
        Ok(Box::pin(stream::unfold(
            (reader, provider, emit_partials, false),
            |(mut reader, provider, emit_partials, mut saw_final)| async move {
                loop {
                    match read_wyoming_event(&mut reader).await {
                        Ok(Some(event)) if event.event_type == "transcript" => {
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
                                    (reader, provider, emit_partials, saw_final),
                                ));
                            }
                            if emit_partials {
                                return Some((
                                    Ok(Transcript::partial(text)),
                                    (reader, provider, emit_partials, saw_final),
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
                                    (reader, provider, emit_partials, saw_final),
                                ));
                            }
                            return None;
                        }
                        Err(error) => {
                            return Some((
                                Err(Error::provider(provider.clone(), error)),
                                (reader, provider, emit_partials, saw_final),
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
    fn transcript_event_without_result_is_partial() {
        let event = WyomingEvent {
            event_type: "transcript".to_owned(),
            data: json!({ "text": "partial text" }),
            payload: Vec::new(),
        };
        let (text, confidence, is_final) = transcript_from_event(&event);
        assert_eq!(text, "partial text");
        assert_eq!(confidence, None);
        assert!(!is_final);
    }
}
