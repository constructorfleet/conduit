//! Wyoming protocol providers for Conduit.
//!
//! [Wyoming](https://github.com/rhasspy/wyoming) is the wire protocol spoken by
//! Rhasspy's speech services — Piper for synthesis, faster-whisper for
//! recognition. Each provider here is a thin client for one capability:
//! [`tts::WyomingTts`] synthesizes text and [`stt::WyomingStt`] recognizes
//! speech, both over a `tcp://host:port` endpoint.

pub mod protocol;
pub mod stt;
pub mod tts;
