//! Text-to-speech over the Wyoming protocol.
//!
//! Piper and other Wyoming TTS servers accept a `synthesize` event carrying
//! the text (and optionally a voice) and answer with `audio-chunk` events,
//! ending with `audio-stop`.

use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::{Error, Result};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{Capability, ChunkStream, Descriptor, Health, Metadata, Provider};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::io::BufReader;
use tokio::net::TcpStream;

use crate::protocol::{read_wyoming_event, tcp_address, write_wyoming_event, CONNECT_TIMEOUT};

/// The one encoding Wyoming audio events carry.
const ENCODINGS: [Encoding; 1] = [Encoding::PcmS16Le];

/// A text-to-speech provider backed by a Wyoming TCP server.
#[derive(Debug, Clone)]
pub struct WyomingTts {
    /// Identity, version, and what this server says it can do.
    descriptor: Descriptor,
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
        // A Wyoming server enumerates its own voices over the protocol, and
        // Conduit does not ask: the definition names the one voice it was
        // configured with, which is the only one an operator chose.
        let voices = voice
            .iter()
            .map(|id| Voice { id: id.clone(), name: id.clone(), language: String::new() })
            .collect();
        let descriptor = Descriptor::new(name, Capability::Tts).with_metadata(
            Metadata::default().with_voices(voices).with_encodings(ENCODINGS.to_vec()),
        );
        Ok(Self { descriptor, address, voice, streaming })
    }

    async fn connect(&self) -> Result<TcpStream> {
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&self.address))
            .await
            .map_err(|_| {
                Error::Config(format!("provider `{}` timed out connecting", self.name()))
            })?
            .map_err(|error| Error::provider(self.name().to_owned(), error))
    }
}

#[async_trait::async_trait]
impl Provider for WyomingTts {
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

        let provider = self.name().to_owned();
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
        let metadata = &provider.descriptor().metadata;
        assert!(metadata.supports_encoding(Encoding::PcmS16Le));
        assert!(!metadata.supports_encoding(Encoding::Opus));
    }

    #[test]
    fn the_configured_voice_is_the_catalogue_the_server_never_offers() {
        // Wyoming has no voice-listing request, so the one voice a definition
        // names is the whole of what an operator screen can show.
        let provider =
            WyomingTts::new("piper", "tcp://localhost:10300", Some("alan".to_owned()), false)
                .unwrap();
        let voices = &provider.descriptor().metadata.voices;
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "alan");
    }
}
