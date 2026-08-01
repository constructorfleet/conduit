//! Text-to-speech over the Wyoming protocol.
//!
//! Piper and other Wyoming TTS servers accept a `synthesize` event carrying
//! the text (and optionally a voice) and answer with `audio-chunk` events,
//! ending with `audio-stop`.

use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::io::BufReader;
use tokio::net::TcpStream;

use crate::protocol::{read_wyoming_event, tcp_address, write_wyoming_event, CONNECT_TIMEOUT};

/// A text-to-speech provider backed by a Wyoming TCP server.
#[derive(Debug, Clone)]
pub struct WyomingTts {
    /// Stable registration name, surfaced in health and diagnostics.
    name: String,
    /// Resolved `host:port` from the `tcp://` URL.
    address: String,
    /// Default voice for synthesis, when the server has one configured.
    voice: Option<String>,
    /// Whether the definition asked for streaming synthesis. Wyoming TTS
    /// always streams audio chunks, so this only affects the stored record,
    /// not the wire protocol.
    #[allow(dead_code)]
    streaming: bool,
}

impl WyomingTts {
    /// Builds a provider for the server at `url`, which must be
    /// `tcp://host:port`. Does not connect.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `url` is not a `tcp://host:port` address.
    pub fn new(
        name: impl Into<String>,
        url: &str,
        voice: Option<String>,
        streaming: bool,
    ) -> Result<Self> {
        let name = name.into();
        let address = tcp_address(url).ok_or_else(|| {
            Error::Config(format!("provider `{name}` Wyoming url must use tcp://host:port"))
        })?;
        Ok(Self { name, address, voice, streaming })
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

#[async_trait::async_trait]
impl Provider for WyomingTts {
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
impl TextToSpeech for WyomingTts {
    async fn synthesize(&self, request: SynthesisRequest) -> Result<ChunkStream<SpeechChunk>> {
        let mut stream = self.connect().await?;
        let voice = request.voice.or_else(|| self.voice.clone());
        let mut data = json!({
            "text": request.text,
            "text_format": "text"
        });
        if let Some(voice) = voice {
            data["voice"] = json!({ "name": voice });
        }
        write_wyoming_event(&mut stream, "synthesize", data).await?;

        let provider = self.name.clone();
        let reader = BufReader::new(stream);
        Ok(Box::pin(stream::unfold(
            (reader, 0_u64, provider),
            |(mut reader, sequence, provider)| async move {
                loop {
                    match read_wyoming_event(&mut reader).await {
                        Ok(Some(event)) if event.event_type == "audio-stop" => return None,
                        Ok(Some(event)) if event.event_type == "audio-chunk" => {
                            let format = AudioFormat {
                                encoding: Encoding::PcmS16Le,
                                sample_rate: event
                                    .data
                                    .get("rate")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(AudioFormat::DEFAULT.sample_rate as u64)
                                    as u32,
                                channels: event
                                    .data
                                    .get("channels")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(AudioFormat::DEFAULT.channels as u64)
                                    as u16,
                            };
                            let chunk = SpeechChunk {
                                sequence,
                                format,
                                data: Bytes::from(event.payload),
                            };
                            return Some((Ok(chunk), (reader, sequence + 1, provider)));
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => return None,
                        Err(error) => {
                            return Some((
                                Err(Error::provider(provider.clone(), error)),
                                (reader, sequence + 1, provider),
                            ));
                        }
                    }
                }
            },
        )))
    }

    async fn voices(&self) -> Result<Vec<Voice>> {
        Ok(Vec::new())
    }

    fn supports_encoding(&self, encoding: Encoding) -> bool {
        encoding == Encoding::PcmS16Le
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::Error;

    #[test]
    fn new_accepts_a_tcp_url() {
        let provider = WyomingTts::new("piper", "tcp://localhost:10300", None, false).unwrap();
        assert_eq!(provider.name(), "piper");
        assert_eq!(provider.address, "localhost:10300");
        assert!(provider.voice.is_none());
        assert!(!provider.streaming);
    }

    #[test]
    fn new_stores_the_default_voice() {
        let provider = WyomingTts::new(
            "piper",
            "tcp://localhost:10300",
            Some("en_US-lessac-low".to_owned()),
            true,
        )
        .unwrap();
        assert_eq!(provider.voice.as_deref(), Some("en_US-lessac-low"));
        assert!(provider.streaming);
    }

    #[test]
    fn new_rejects_non_tcp_urls() {
        let error =
            WyomingTts::new("piper", "http://localhost:10300", None, false).unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn new_rejects_malformed_tcp_urls() {
        let error = WyomingTts::new("piper", "tcp://", None, false).unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn supports_only_pcm_s16_le() {
        let provider = WyomingTts::new("piper", "tcp://localhost:10300", None, false).unwrap();
        assert!(provider.supports_encoding(Encoding::PcmS16Le));
        assert!(!provider.supports_encoding(Encoding::Opus));
    }
}
