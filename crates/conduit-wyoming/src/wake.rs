//! Wake word detection over the Wyoming protocol.
//!
//! Wyoming wake servers — openWakeWord, nanoWakeWord, and microWakeWord all
//! ship one — accept a `detect` event naming the phrases to listen for,
//! followed by the same `audio-start` / `audio-chunk` / `audio-stop` sequence
//! recognition uses. They answer with a `detection` event naming the phrase
//! that fired, or with `not-detected` when the audio ended without one.
//!
//! The session outlives a single activation: the microphone keeps listening
//! after the assistant answers, so a detection is an item on the stream rather
//! than the end of it.

use conduit_core::{Error, Result};
use conduit_provider::stt::AudioChunk;
use conduit_provider::wake::{Detection, WakePhrase, WakeWordDetector};
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::io::BufReader;
use tokio::net::TcpStream;

use crate::protocol::CONNECT_TIMEOUT;
use crate::protocol::{
    read_wyoming_event, tcp_address, write_wyoming_event, write_wyoming_event_with_payload,
    WyomingEvent,
};

/// The event a server sends when a phrase fired.
const DETECTION: &str = "detection";
/// The event a server sends when the audio ended without one.
const NOT_DETECTED: &str = "not-detected";

/// A wake word detector backed by a Wyoming TCP server.
#[derive(Debug, Clone)]
pub struct WyomingWake {
    /// Stable registration name, surfaced in health and diagnostics.
    name: String,
    /// Resolved `host:port` from the `tcp://` URL.
    address: String,
    /// Phrases this definition was configured with, offered to callers that
    /// do not name their own.
    phrases: Vec<String>,
}

impl WyomingWake {
    /// Builds a detector for the server at `url`, which must be
    /// `tcp://host:port`. Does not connect.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `url` is not a `tcp://host:port` address.
    pub fn new(name: impl Into<String>, url: &str, phrases: Vec<String>) -> Result<Self> {
        let name = name.into();
        let address = tcp_address(url).ok_or_else(|| {
            Error::Config(format!("provider `{name}` Wyoming url must use tcp://host:port"))
        })?;
        Ok(Self { name, address, phrases })
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

/// Reads a detection event as a phrase, a confidence, and whether it was
/// accepted.
///
/// The protocol guarantees only the phrase name: a server that fired has
/// already applied its own threshold, so a detection with no score is accepted
/// at full confidence rather than discarded. Servers that do report one —
/// openWakeWord calls it `probability`, others `score` — are scored against the
/// phrase's threshold, which is what lets an operator tighten a detector that
/// is firing at the television.
fn detection_from_event(event: &WyomingEvent, phrases: &[WakePhrase]) -> Detection {
    let phrase = event.data.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
    let reported = event
        .data
        .get("probability")
        .or_else(|| event.data.get("score"))
        .and_then(Value::as_f64);
    let confidence = reported.unwrap_or(1.0) as f32;
    let threshold = phrases
        .iter()
        .find(|candidate| candidate.phrase == phrase)
        .map_or(0.0, |candidate| candidate.threshold);
    Detection { phrase, confidence, accepted: confidence >= threshold }
}

#[async_trait::async_trait]
impl Provider for WyomingWake {
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
impl WakeWordDetector for WyomingWake {
    async fn detect(
        &self,
        audio: ChunkStream<AudioChunk>,
        phrases: Vec<WakePhrase>,
    ) -> Result<ChunkStream<Detection>> {
        let stream = self.connect().await?;
        let (read_half, mut write_half) = stream.into_split();

        // An empty `names` asks the server to score everything it loaded,
        // which is what a definition that named no phrases means.
        let names: Vec<&str> = phrases.iter().map(|phrase| phrase.phrase.as_str()).collect();
        write_wyoming_event(&mut write_half, "detect", json!({ "names": names })).await?;

        // Wyoming reads the format off every `audio-chunk` as well as off
        // `audio-start`, so the same description is repeated on each one.
        let chunk_format = json!({ "rate": 16000, "width": 2, "channels": 1 });
        let mut start = chunk_format.clone();
        start["encoding"] = json!("pcm_s16le");
        write_wyoming_event(&mut write_half, "audio-start", start).await?;

        // Held by both the pump and the detection stream for the same reason
        // recognition holds its writer: dropping the write half is a FIN, and
        // an asyncio server tears the whole connection down when it sees one —
        // losing detections it had already produced.
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
                        if let Err(error) = write_wyoming_event_with_payload(
                            write_half,
                            "audio-chunk",
                            chunk_format.clone(),
                            &chunk.data,
                        )
                        .await
                        {
                            tracing::warn!(
                                provider = pump_provider,
                                sequence = chunk.sequence,
                                error = %error,
                                "failed to forward audio to the wake detector"
                            );
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            provider = pump_provider,
                            error = %error,
                            "audio input failed; closing the wake session"
                        );
                        return;
                    }
                }
            }
            if let Err(error) = write_wyoming_event(write_half, "audio-stop", json!({})).await {
                tracing::warn!(
                    provider = pump_provider,
                    error = %error,
                    "failed to signal end of audio to the wake detector"
                );
            }
        });

        let reader = BufReader::new(read_half);
        Ok(Box::pin(stream::unfold(
            (reader, provider, phrases, writer),
            |(mut reader, provider, phrases, writer)| async move {
                loop {
                    match read_wyoming_event(&mut reader).await {
                        Ok(Some(event)) if event.event_type == DETECTION => {
                            let detection = detection_from_event(&event, &phrases);
                            tracing::info!(
                                provider,
                                phrase = %detection.phrase,
                                confidence = detection.confidence,
                                accepted = detection.accepted,
                                "wake detector scored an activation"
                            );
                            return Some((Ok(detection), (reader, provider, phrases, writer)));
                        }
                        Ok(Some(event)) if event.event_type == NOT_DETECTED => {
                            // The audio ended without an activation. Nothing was
                            // scored, so there is nothing to report and nothing
                            // further to read.
                            tracing::debug!(provider, "audio ended with no wake word");
                            return None;
                        }
                        Ok(Some(event)) => {
                            tracing::debug!(
                                provider,
                                event = %event.event_type,
                                "ignoring a Wyoming event that is not a detection"
                            );
                        }
                        // A closed connection ends the session. Unlike a
                        // transcript there is nothing outstanding: a detector
                        // that heard nothing owes no answer.
                        Ok(None) => return None,
                        Err(error) => {
                            return Some((
                                Err(Error::provider(provider.clone(), error)),
                                (reader, provider, phrases, writer),
                            ))
                        }
                    }
                }
            },
        )))
    }

    fn available_phrases(&self) -> &[String] {
        &self.phrases
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A server that answers one session with `events`, after reading the
    /// client's `detect` and audio.
    async fn serve(listener: TcpListener, events: Vec<(&'static str, Value)>) {
        let (stream, _) = listener.accept().await.expect("provider connects");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
            if event.event_type == "audio-stop" {
                break;
            }
        }
        for (event_type, data) in events {
            write_wyoming_event(&mut write_half, event_type, data)
                .await
                .expect("event written");
        }
        // Held open, as a real server keeps listening for the next activation.
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }

    fn audio() -> ChunkStream<AudioChunk> {
        Box::pin(stream::iter([Ok(AudioChunk { sequence: 0, data: vec![1, 2, 3, 4].into() })]))
    }

    #[tokio::test]
    async fn a_detection_names_the_phrase_that_fired() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(serve(
            listener,
            vec![(DETECTION, json!({ "name": "hey jarvis", "probability": 0.91 }))],
        ));

        let provider =
            WyomingWake::new("openwakeword", &format!("tcp://{address}"), Vec::new())
                .expect("built");
        let mut detections = provider
            .detect(audio(), vec![WakePhrase::new("hey jarvis")])
            .await
            .expect("session");

        let first = detections.next().await.expect("a detection").expect("not an error");
        assert_eq!(first.phrase, "hey jarvis");
        assert!((first.confidence - 0.91).abs() < f32::EPSILON);
        assert!(first.accepted, "0.91 clears the conventional 0.5 threshold");
        server.abort();
    }

    #[tokio::test]
    async fn a_near_miss_is_reported_rather_than_swallowed() {
        // Near misses are how an operator tunes sensitivity, so a score below
        // the phrase's threshold arrives as a rejected detection rather than
        // as silence.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(serve(
            listener,
            vec![(DETECTION, json!({ "name": "hey jarvis", "probability": 0.2 }))],
        ));

        let provider =
            WyomingWake::new("openwakeword", &format!("tcp://{address}"), Vec::new())
                .expect("built");
        let mut detections = provider
            .detect(audio(), vec![WakePhrase::new("hey jarvis").with_threshold(0.8)])
            .await
            .expect("session");

        let first = detections.next().await.expect("a detection").expect("not an error");
        assert!(!first.accepted);
        assert_eq!(first.phrase, "hey jarvis");
        server.abort();
    }

    #[tokio::test]
    async fn a_detection_without_a_score_is_accepted_at_full_confidence() {
        // The protocol only guarantees the phrase name. A server that fired has
        // already applied its own threshold, so treating a missing score as
        // zero would discard every detection microWakeWord ever sends.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server =
            tokio::spawn(serve(listener, vec![(DETECTION, json!({ "name": "okay nabu" }))]));

        let provider =
            WyomingWake::new("microwakeword", &format!("tcp://{address}"), Vec::new())
                .expect("built");
        let mut detections = provider
            .detect(audio(), vec![WakePhrase::new("okay nabu").with_threshold(0.9)])
            .await
            .expect("session");

        let first = detections.next().await.expect("a detection").expect("not an error");
        assert!(first.accepted);
        assert!((first.confidence - 1.0).abs() < f32::EPSILON);
        server.abort();
    }

    #[tokio::test]
    async fn audio_that_ends_without_an_activation_ends_the_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(serve(listener, vec![(NOT_DETECTED, json!({}))]));

        let provider =
            WyomingWake::new("openwakeword", &format!("tcp://{address}"), Vec::new())
                .expect("built");
        let mut detections = provider
            .detect(audio(), vec![WakePhrase::new("hey jarvis")])
            .await
            .expect("session");

        let ended = tokio::time::timeout(std::time::Duration::from_secs(2), detections.next())
            .await
            .expect("the stream must end rather than wait for the server to hang up");
        assert!(ended.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn the_phrases_to_listen_for_are_named_before_any_audio() {
        // A server scores what `detect` asked for. Sending audio first, or not
        // sending `detect` at all, leaves it scoring whatever it happened to
        // load — which is a pipeline that wakes on a phrase nobody configured.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("provider connects");
            let mut reader = BufReader::new(stream);
            let mut seen = Vec::new();
            while let Ok(Some(event)) = read_wyoming_event(&mut reader).await {
                let stop = event.event_type == "audio-stop";
                seen.push(event);
                if stop {
                    break;
                }
            }
            seen
        });

        let provider =
            WyomingWake::new("openwakeword", &format!("tcp://{address}"), Vec::new())
                .expect("built");
        let _detections = provider
            .detect(audio(), vec![WakePhrase::new("hey jarvis"), WakePhrase::new("okay nabu")])
            .await
            .expect("session");

        let events = server.await.expect("server task");
        assert_eq!(events[0].event_type, "detect", "phrases are named first");
        assert_eq!(
            events[0].data.get("names").and_then(Value::as_array).map(Vec::len),
            Some(2)
        );
        assert_eq!(events[1].event_type, "audio-start");
    }

    #[test]
    fn new_rejects_non_tcp_urls() {
        let error =
            WyomingWake::new("openwakeword", "ws://localhost:10400", Vec::new()).unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn configured_phrases_are_offered_to_callers_that_name_none() {
        let provider = WyomingWake::new(
            "openwakeword",
            "tcp://localhost:10400",
            vec!["hey jarvis".to_owned()],
        )
        .expect("built");
        assert_eq!(provider.available_phrases(), ["hey jarvis"]);
    }
}
