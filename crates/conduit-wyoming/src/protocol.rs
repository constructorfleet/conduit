//! Wyoming wire protocol helpers.
//!
//! Events are newline-delimited JSON headers, optionally followed by extra
//! JSON `data` bytes and a binary payload:
//!
//! ```text
//! {"type":"<event>","data":{...},"data_length":0,"payload_length":0}\n
//! ```
//!
//! When `data_length` is non-zero that many bytes of JSON follow the header
//! and are deep-merged into `data`; when `payload_length` is non-zero that
//! many raw bytes follow as the event payload (audio samples). The helpers are
//! generic over the underlying stream so they can be exercised in unit tests
//! over `tokio::io::duplex`.

use std::time::Duration;

use conduit_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// How long a provider waits for its TCP connection to be established.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// One parsed Wyoming event: its type, its merged `data` object, and any raw
/// binary payload carried after the header.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WyomingEvent {
    /// The event kind, e.g. `"synthesize"` or `"audio-chunk"`.
    pub(crate) event_type: String,
    /// Header `data`, with any separately-framed `data_length` JSON merged in.
    pub(crate) data: Value,
    /// Raw payload bytes following the header, empty when `payload_length` is
    /// zero.
    pub(crate) payload: Vec<u8>,
}

/// The JSON header of a Wyoming event.
#[derive(Debug, Deserialize)]
struct WyomingHeader {
    /// The event kind.
    #[serde(rename = "type")]
    event_type: String,
    /// Inline event data.
    #[serde(default)]
    data: Value,
    /// Bytes of JSON to read and merge into `data`.
    #[serde(default)]
    data_length: usize,
    /// Bytes of raw payload to read after the header.
    #[serde(default)]
    payload_length: usize,
}

/// Writes a data-only Wyoming event: a JSON header line with no payload.
///
/// # Errors
///
/// Returns [`Error::Provider`] if the header cannot be written.
pub(crate) async fn write_wyoming_event<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
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

/// Writes a Wyoming event with a binary payload, e.g. `audio-chunk` carrying
/// encoded samples.
///
/// # Errors
///
/// Returns [`Error::Provider`] if the header or payload cannot be written.
pub(crate) async fn write_wyoming_event_with_payload<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    event_type: &str,
    data: Value,
    payload: &[u8],
) -> Result<()> {
    let header = json!({
        "type": event_type,
        "data": data,
        "data_length": 0,
        "payload_length": payload.len(),
    });
    stream
        .write_all(header.to_string().as_bytes())
        .await
        .map_err(|error| Error::provider("wyoming", error))?;
    stream.write_all(b"\n").await.map_err(|error| Error::provider("wyoming", error))?;
    stream.write_all(payload).await.map_err(|error| Error::provider("wyoming", error))
}

/// Reads one Wyoming event from `reader`.
///
/// Returns `None` at end of stream (a zero-byte header line). A non-zero
/// `data_length` is read as JSON and merged object-key-wise into the header
/// `data`; a non-zero `payload_length` is read as raw bytes.
///
/// # Errors
///
/// Returns [`Error::Config`] for a malformed header or extra data, and
/// [`Error::Provider`] for I/O failures.
pub(crate) async fn read_wyoming_event<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut tokio::io::BufReader<R>,
) -> Result<Option<WyomingEvent>> {
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
    let mut payload = Vec::with_capacity(header.payload_length);
    if header.payload_length > 0 {
        payload.resize(header.payload_length, 0);
        reader
            .read_exact(&mut payload)
            .await
            .map_err(|error| Error::provider("wyoming", error))?;
    }

    Ok(Some(WyomingEvent { event_type: header.event_type, data, payload }))
}

/// Merges the object keys of `extra` into `base`.
///
/// A `base` that is not an object is replaced outright. Wyoming serializes
/// `data` into its own framed bytes and drops the `data` key from the header
/// line, so the base is usually the `null` a missing key deserializes to —
/// and merging into it object-key-wise silently discarded the entire event.
fn merge_data(base: &mut Value, extra: Value) {
    let Some(target) = base.as_object_mut() else {
        *base = extra;
        return;
    };
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// The event type a Wyoming server sends to refuse a request.
///
/// A server that will not serve a session says why and then closes. Reading it
/// is the difference between an operator seeing "sample rate 16000 is not
/// supported, expected 48000" and seeing a closed connection.
pub(crate) const ERROR: &str = "error";

/// A refusal a Wyoming server sent, carried as the source of an
/// [`Error::Provider`].
///
/// A distinct type rather than an [`std::io::Error`]: this is the server
/// answering, not the transport failing, and `Error::Provider`'s display puts
/// the source text straight in front of the operator.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct WyomingServerError(String);

impl WyomingServerError {
    /// Wraps the message a server sent.
    pub(crate) const fn new(message: String) -> Self {
        Self(message)
    }
}

/// Renders a Wyoming `error` event as the message an operator should read.
///
/// The protocol carries `text` and, on some servers, `code`. Both are optional
/// in practice, so a server that sends an `error` with neither still produces a
/// message that says an error arrived — reporting nothing at all would leave
/// the caller with the closed-connection story this exists to replace.
pub(crate) fn error_message(event: &WyomingEvent) -> String {
    let text = event.data.get("text").and_then(Value::as_str).filter(|text| !text.is_empty());
    let code = event.data.get("code").and_then(Value::as_str).filter(|code| !code.is_empty());
    match (text, code) {
        (Some(text), Some(code)) => format!("{text} (code {code})"),
        (Some(text), None) => text.to_owned(),
        (None, Some(code)) => format!("server reported error code {code}"),
        (None, None) => "server reported an error with no message".to_owned(),
    }
}

/// Parses `tcp://host:port` into `host:port`, or `None` for any other scheme.
pub(crate) fn tcp_address(url: &str) -> Option<String> {
    url.strip_prefix("tcp://")
        .filter(|address| !address.is_empty() && address.contains(':'))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn data_framed_separately_survives_a_header_that_omits_it() {
        // How Wyoming actually writes: `data` is serialized to its own bytes,
        // `data_length` is set, and the `data` key is dropped from the header
        // line. Merging that into a header whose `data` defaulted to null
        // silently discarded all of it, so a real transcript arrived as empty
        // text. Our own writer inlines `data`, so a round-trip through it
        // never exercised this.
        let (mut client, server) = duplex(1024);
        let data = br#"{"text": "what time is it"}"#;
        let header = format!("{{\"type\": \"transcript\", \"data_length\": {}}}\n", data.len());
        client.write_all(header.as_bytes()).await.unwrap();
        client.write_all(data).await.unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let event = read_wyoming_event(&mut reader).await.unwrap().expect("one event");
        assert_eq!(event.event_type, "transcript");
        assert_eq!(
            event.data.get("text").and_then(Value::as_str),
            Some("what time is it"),
            "framed data must survive a header with no `data` key"
        );
    }

    #[tokio::test]
    async fn round_trips_an_event_with_data_only() {
        let (mut client, server) = duplex(1024);
        let data = json!({ "text": "hello", "text_format": "text" });

        write_wyoming_event(&mut client, "synthesize", data.clone()).await.unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let event = read_wyoming_event(&mut reader).await.unwrap().expect("one event");
        assert_eq!(event.event_type, "synthesize");
        assert_eq!(event.data, data);
        assert!(event.payload.is_empty());
    }

    #[tokio::test]
    async fn round_trips_an_event_with_a_binary_payload() {
        let (mut client, server) = duplex(1024);
        let payload = b"\x00\xff\x10\x20".to_vec();

        write_wyoming_event_with_payload(&mut client, "audio-chunk", json!({}), &payload)
            .await
            .unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let event = read_wyoming_event(&mut reader).await.unwrap().expect("one event");
        assert_eq!(event.event_type, "audio-chunk");
        assert!(event.data.is_object());
        assert_eq!(event.payload, payload);
    }

    #[tokio::test]
    async fn read_returns_none_at_end_of_stream() {
        let (client, server) = duplex(1024);
        drop(client);

        let mut reader = BufReader::new(server);
        assert!(read_wyoming_event(&mut reader).await.unwrap().is_none());
    }

    #[test]
    fn tcp_address_accepts_tcp_urls() {
        assert_eq!(tcp_address("tcp://host:10300"), Some("host:10300".to_owned()));
    }

    #[test]
    fn tcp_address_rejects_other_schemes() {
        assert_eq!(tcp_address("http://host:10300"), None);
    }

    #[test]
    fn tcp_address_rejects_malformed_urls() {
        assert_eq!(tcp_address("tcp://"), None);
        assert_eq!(tcp_address("tcp://noport"), None);
    }
}
