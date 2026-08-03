//! Wyoming protocol providers for Conduit.
//!
//! [Wyoming](https://github.com/rhasspy/wyoming) is the wire protocol spoken by
//! Rhasspy's speech services — Piper for synthesis, faster-whisper for
//! recognition, openWakeWord for activation. Each provider here is a thin
//! client for one capability: [`tts::WyomingTts`] synthesizes text,
//! [`stt::WyomingStt`] recognizes speech, and [`wake::WyomingWake`] listens for
//! a wake phrase — all over a `tcp://host:port` endpoint.

pub mod protocol;
pub mod stt;
pub mod tts;
pub mod wake;
