//! Server-sent event framing.
//!
//! Hand-rolled rather than pulled in as a dependency: the subset that matters
//! here is one field (`data:`), one terminator (a blank line), and one
//! sentinel (`[DONE]`).

/// Reassembles SSE payloads from arbitrarily split byte packets.
///
/// TCP does not preserve message boundaries, so a chunk of JSON may arrive in
/// pieces or several may arrive together. Feed every packet in and take
/// whatever complete payloads come out.
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

    /// Adds a packet and returns the payloads it completed.
    ///
    /// Takes bytes rather than text because a packet can split a multi-byte
    /// character; decoding is deferred until a whole event is in hand.
    pub fn push(&mut self, packet: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(packet);

        let mut payloads = Vec::new();
        while let Some((end, width)) = terminator(&self.buffer) {
            let event: Vec<u8> = self.buffer.drain(..end + width).collect();
            let event = String::from_utf8_lossy(&event);
            if let Some(data) = payload(&event) {
                payloads.push(data);
            }
        }
        payloads
    }
}

/// Where the first event ends, and how many bytes its terminator takes.
///
/// An event ends at a blank line, and the spec allows CRLF, LF, or CR line
/// endings. Looking only for `\n\n` finds nothing in a CRLF stream, which is
/// not a parse error anyone sees: the buffer grows until the stream ends and
/// the response appears never to have arrived.
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

/// Extracts the `data:` content of one event, ignoring comments and other
/// fields, and returning `None` for the `[DONE]` sentinel.
fn payload(event: &str) -> Option<String> {
    let mut data = String::new();
    for line in event.lines() {
        // A multi-line data field is joined with newlines, per the spec.
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }

    if data.is_empty() || data == "[DONE]" {
        None
    } else {
        Some(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_separated_by_crlf_are_decoded() {
        // A provider using CRLF produced no payloads at all: the terminator
        // was never found, so the stream read as one that answered nothing.
        let mut decoder = Decoder::new();

        assert_eq!(decoder.push(b"data: {\"a\":1}\r\n\r\n"), ["{\"a\":1}"]);
    }

    #[test]
    fn a_whole_event_yields_its_payload() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.push(b"data: {\"a\":1}\n\n"), ["{\"a\":1}"]);
    }

    #[test]
    fn several_events_in_one_packet_all_come_out() {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.push(b"data: one\n\ndata: two\n\n"), ["one", "two"]);
    }

    #[test]
    fn a_split_event_is_held_until_complete() {
        let mut decoder = Decoder::new();
        assert!(decoder.push(b"data: {\"a\"").is_empty());
        assert!(decoder.push(b":1}").is_empty());
        assert_eq!(decoder.push(b"\n\n"), ["{\"a\":1}"]);
    }

    #[test]
    fn the_done_sentinel_is_not_a_payload() {
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
        assert_eq!(decoder.push(b"data: one\ndata: two\n\n"), ["one\ntwo"]);
    }
}
