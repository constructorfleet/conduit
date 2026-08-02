//! Server-sent event framing.
//!
//! Hand-rolled rather than pulled in as a dependency: the subset that matters
//! here is the `event:` name, the `data:` field, and the blank-line
//! terminator. The decoder is a port of the one in `conduit-openai`, extended
//! to report the event name because MCP's SSE transport keys on it
//! (`endpoint` and `message` events).

/// One parsed SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, defaulting to `message` per the SSE spec.
    pub name: String,
    /// The concatenated `data:` lines.
    pub data: String,
}

/// Reassembles SSE events from arbitrarily split byte packets.
///
/// TCP does not preserve message boundaries, so a chunk of JSON may arrive in
/// pieces or several may arrive together. Feed every packet in and take
/// whatever complete events come out.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    /// A decoder with an empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a packet and returns the events it completed.
    ///
    /// Takes bytes rather than text because a packet can split a multi-byte
    /// character; decoding is deferred until a whole event is in hand.
    /// Events with no data and the `[DONE]` sentinel are dropped.
    pub fn push(&mut self, packet: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(packet);

        let mut events = Vec::new();
        while let Some((end, width)) = terminator(&self.buffer) {
            let raw: Vec<u8> = self.buffer.drain(..end + width).collect();
            let raw = String::from_utf8_lossy(&raw);
            if let Some(event) = parse(&raw) {
                events.push(event);
            }
        }
        events
    }
}

/// Where the first event ends, and how many bytes its terminator takes.
///
/// An event ends at a blank line, and the spec allows CRLF, LF, or CR line
/// endings — so the blank line is any of `\r\n\r\n`, `\n\n`, or `\r\r`. Looking
/// only for `\n\n` finds nothing in a CRLF stream, which is not a parse error
/// anyone sees: the buffer simply grows until the stream ends and the reply
/// appears never to have arrived.
///
/// The earliest terminator wins, and CRLF is preferred where they start at the
/// same byte so its trailing `\n` is not left to open the next event.
fn terminator(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut found: Option<(usize, usize)> = None;
    for (needle, width) in [(&b"\r\n\r\n"[..], 4), (&b"\n\n"[..], 2), (&b"\r\r"[..], 2)] {
        if let Some(at) = find(buffer, needle) {
            found = match found {
                Some((best, _)) if best <= at => found,
                _ => Some((at, width)),
            };
        }
    }
    found
}

/// Offset of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Parses one raw event, ignoring comments and unknown fields, and returning
/// `None` when the event carries no data or is the `[DONE]` sentinel.
fn parse(raw: &str) -> Option<SseEvent> {
    let mut name = "message".to_owned();
    let mut data = String::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            let value = rest.trim_start();
            if !value.is_empty() {
                name = value.to_owned();
            }
        } else if let Some(rest) = line.strip_prefix("data:") {
            // A multi-line data field is joined with newlines, per the spec.
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
        // Lines starting with ':' are comments; other fields are ignored.
    }

    if data.is_empty() || data == "[DONE]" {
        None
    } else {
        Some(SseEvent { name, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_event_yields_its_payload() {
        let mut decoder = Decoder::new();
        let events = decoder.push(b"data: {\"a\":1}\n\n");
        assert_eq!(events, [SseEvent { name: "message".into(), data: "{\"a\":1}".into() }]);
    }

    #[test]
    fn several_events_in_one_packet_all_come_out() {
        let mut decoder = Decoder::new();
        let events = decoder.push(b"data: one\n\ndata: two\n\n");
        assert_eq!(
            events,
            [
                SseEvent { name: "message".into(), data: "one".into() },
                SseEvent { name: "message".into(), data: "two".into() },
            ]
        );
    }

    #[test]
    fn a_split_event_is_held_until_complete() {
        let mut decoder = Decoder::new();
        assert!(decoder.push(b"data: {\"a\"").is_empty());
        assert!(decoder.push(b":1}").is_empty());
        assert_eq!(decoder.push(b"\n\n").len(), 1);
    }

    #[test]
    fn the_done_sentinel_is_not_an_event() {
        let mut decoder = Decoder::new();
        assert!(decoder.push(b"data: [DONE]\n\n").is_empty());
    }

    #[test]
    fn comments_and_keepalives_are_ignored() {
        let mut decoder = Decoder::new();
        assert!(decoder.push(b": keepalive\n\n").is_empty());
    }

    #[test]
    fn multi_line_data_is_joined() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.push(b"data: one\ndata: two\n\n")[0].data, "one\ntwo");
    }

    #[test]
    fn events_separated_by_crlf_are_decoded() {
        // The SSE spec allows CRLF, and sse-starlette — which the Python MCP
        // SDK serves streamable HTTP through — uses it. Splitting only on
        // "\n\n" found no terminator in "\r\n\r\n", so every event stayed in
        // the buffer and the stream looked like it answered nothing.
        let mut decoder = Decoder::new();
        let events = decoder.push(b"event: message\r\ndata: {\"a\":1}\r\n\r\n");

        assert_eq!(events, [SseEvent { name: "message".into(), data: "{\"a\":1}".into() }]);
    }

    #[test]
    fn a_crlf_event_split_across_packets_is_reassembled() {
        let mut decoder = Decoder::new();
        assert!(decoder.push(b"event: message\r\ndata: {\"a\":").is_empty());
        let events = decoder.push(b"1}\r\n\r\n");

        assert_eq!(events, [SseEvent { name: "message".into(), data: "{\"a\":1}".into() }]);
    }

    #[test]
    fn the_event_name_is_captured() {
        let mut decoder = Decoder::new();
        let events = decoder.push(b"event: endpoint\ndata: /messages\n\n");
        assert_eq!(events, [SseEvent { name: "endpoint".into(), data: "/messages".into() }]);
    }
}
