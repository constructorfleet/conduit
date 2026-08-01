//! Inline Wyoming providers carried by pipeline node configuration.
//!
//! The Operator Console can define provider instances before Conduit has a
//! server-side provider store. Those instances become runnable only when their
//! runtime config is saved into the graph node that selects them.

use std::time::Duration;

use bytes::Bytes;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::graph::{Node, NodeKind};
use conduit_core::{Error, Result};
use conduit_provider::tts::{SpeechChunk, SynthesisRequest, TextToSpeech, Voice};
use conduit_provider::{ChunkStream, Health, Provider};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Provider configuration embedded in a graph node by the Operator Console.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct InlineProviderConfig {
    /// Runtime component identifier, such as `wyoming.tts`.
    pub component: Option<String>,
    /// Wyoming TCP endpoint, written as `tcp://host:port`.
    pub url: Option<String>,
    /// Voice identifier for synthesis.
    pub voice: Option<String>,
    /// Backward-compatible alias for voice/model-like UI fields.
    pub model: Option<String>,
    /// Backward-compatible alias for older UI field names.
    pub mode: Option<String>,
}

impl InlineProviderConfig {
    /// Whether this config describes a Wyoming text-to-speech provider.
    pub fn is_wyoming_tts(&self) -> bool {
        matches!(self.component.as_deref(), Some("wyoming.tts") | Some("wyoming"))
    }

    fn voice(&self) -> Option<String> {
        self.voice.clone().or_else(|| self.model.clone()).or_else(|| self.mode.clone())
    }
}

/// Text-to-speech provider backed by a Wyoming TCP server.
#[derive(Debug, Clone)]
pub struct WyomingTts {
    name: String,
    address: String,
    voice: Option<String>,
}

impl WyomingTts {
    /// Builds a provider from inline node configuration.
    pub fn from_inline(name: &str, config: InlineProviderConfig) -> Result<Self> {
        if !config.is_wyoming_tts() {
            return Err(Error::Config(format!(
                "provider `{name}` is not a Wyoming TTS provider"
            )));
        }
        let url = config
            .url
            .clone()
            .ok_or_else(|| Error::Config(format!("provider `{name}` needs a Wyoming `url`")))?;
        let address = tcp_address(&url).ok_or_else(|| {
            Error::Config(format!("provider `{name}` Wyoming url must use tcp://host:port"))
        })?;

        Ok(Self { name: name.to_owned(), address, voice: config.voice() })
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

/// Builds an inline Wyoming TTS provider when `node` carries matching config.
pub fn wyoming_tts_from_node(node: &Node) -> Result<Option<WyomingTts>> {
    if node.kind != NodeKind::Tts || node.config.is_null() {
        return Ok(None);
    }
    let config: InlineProviderConfig =
        serde_json::from_value(node.config.clone()).map_err(|error| {
            Error::Config(format!("node `{}` has invalid configuration: {error}", node.id))
        })?;
    if !config.is_wyoming_tts() {
        return Ok(None);
    }
    WyomingTts::from_inline(&node.provider, config).map(Some)
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
                                    .and_then(serde_json::Value::as_u64)
                                    .unwrap_or(AudioFormat::DEFAULT.sample_rate as u64)
                                    as u32,
                                channels: event
                                    .data
                                    .get("channels")
                                    .and_then(serde_json::Value::as_u64)
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

#[derive(Debug)]
struct WyomingEvent {
    event_type: String,
    data: Value,
    payload: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct WyomingHeader {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    data_length: usize,
    #[serde(default)]
    payload_length: usize,
}

async fn write_wyoming_event(
    stream: &mut TcpStream,
    event_type: &str,
    data: Value,
) -> Result<()> {
    let header = json!({ "type": event_type, "data": data });
    stream
        .write_all(header.to_string().as_bytes())
        .await
        .map_err(|error| Error::provider("wyoming", error))?;
    stream.write_all(b"\n").await.map_err(|error| Error::provider("wyoming", error))
}

async fn read_wyoming_event(reader: &mut BufReader<TcpStream>) -> Result<Option<WyomingEvent>> {
    let mut line = String::new();
    let read =
        reader.read_line(&mut line).await.map_err(|error| Error::provider("wyoming", error))?;
    if read == 0 {
        return Ok(None);
    }

    let header: WyomingHeader = serde_json::from_str(line.trim_end())
        .map_err(|error| Error::Config(format!("invalid Wyoming event header: {error}")))?;
    let mut data = header.data;
    if header.data_length > 0 {
        let mut data_bytes = vec![0_u8; header.data_length];
        reader
            .read_exact(&mut data_bytes)
            .await
            .map_err(|error| Error::provider("wyoming", error))?;
        let extra: Value = serde_json::from_slice(&data_bytes)
            .map_err(|error| Error::Config(format!("invalid Wyoming event data: {error}")))?;
        merge_data(&mut data, extra);
    }
    let mut payload = vec![0_u8; header.payload_length];
    if header.payload_length > 0 {
        reader
            .read_exact(&mut payload)
            .await
            .map_err(|error| Error::provider("wyoming", error))?;
    }

    Ok(Some(WyomingEvent { event_type: header.event_type, data, payload }))
}

fn merge_data(base: &mut Value, extra: Value) {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
}

fn tcp_address(url: &str) -> Option<String> {
    url.strip_prefix("tcp://")
        .filter(|address| !address.is_empty() && address.contains(':'))
        .map(str::to_owned)
}
