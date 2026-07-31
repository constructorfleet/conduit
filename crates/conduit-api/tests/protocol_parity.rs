//! Keeping the firmware's copy of the converse protocol honest.
//!
//! The protocol is defined twice: canonically in Rust, and again in the
//! shipped ESPHome header, because a header-only C++ contract cannot be
//! generated from `serde`. Two checks stand in for that:
//!
//! - the constants must agree, which catches a rename or a changed value;
//! - the firmware parser must read the exact bytes this server emits, which is
//!   the part that matters, and which lives in a fixture the firmware test
//!   suite consumes.
//!
//! The fixture is checked in so the firmware suite runs without a Rust build
//! first. This test regenerates it and fails if the checked-in copy differs,
//! which is what stops it from being quietly hand-edited.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use conduit_core::audio::AudioFormat;
use conduit_core::device::{Command, Notice};
use conduit_core::id::ConversationId;

/// Environment variable that turns this test into a generator.
const REGENERATE: &str = "CONDUIT_REGENERATE_FIXTURES";

/// A fixed id, so a regenerated fixture differs only when the protocol does.
const CONVERSATION: &str = "0f1e2d3c-4b5a-4978-8796-a5b4c3d2e1f0";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn fixture_path() -> PathBuf {
    repo_root().join("firmware/tests/notices.fixture")
}

fn firmware_header() -> String {
    let path = repo_root()
        .join("firmware/esphome/components/conduit_voice/conduit_converse_embedded.h");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"))
}

/// One frame, and what the firmware parser must make of it.
struct Record {
    /// Names the case in the fixture and in a failure message.
    name: &'static str,
    /// The text frame exactly as it goes on the wire.
    frame: String,
    /// The notice type the firmware must report.
    expect_type: &'static str,
    /// The decoded `conversation` value, if the frame carries one.
    conversation: Option<String>,
    /// The decoded `error` value, if the frame carries one.
    error: Option<String>,
}

impl Record {
    fn new(name: &'static str, frame: String, expect_type: &'static str) -> Self {
        Self { name, frame, expect_type, conversation: None, error: None }
    }

    fn with_conversation(mut self, conversation: &str) -> Self {
        self.conversation = Some(conversation.to_owned());
        self
    }

    fn with_error(mut self, error: &str) -> Self {
        self.error = Some(error.to_owned());
        self
    }
}

/// Serializes a notice the way the conversation socket does.
fn frame(notice: &Notice) -> String {
    serde_json::to_string(notice).expect("a notice serializes")
}

/// Error text that breaks a naive scanner.
///
/// It contains a quote, a backslash, and the literal key pattern the firmware
/// looks for to identify a notice. None of that is contrived: this field is
/// filled from a provider's error message, so an upstream server's wording
/// decides what a satellite parses.
const HOSTILE_ERROR: &str = r#"provider said "type":"done" while loading C:\models\voice.onnx"#;

/// Every frame the firmware must handle, canonical ones first.
fn records() -> Vec<Record> {
    let conversation =
        ConversationId::from_uuid(CONVERSATION.parse().expect("a valid uuid literal"));

    let mut records = vec![
        Record::new("started", frame(&Notice::Started { conversation }), "started")
            .with_conversation(CONVERSATION),
        Record::new("done", frame(&Notice::Done), "done"),
        Record::new(
            "failed",
            frame(&Notice::Failed { error: "the model provider is offline".to_owned() }),
            "failed",
        )
        .with_error("the model provider is offline"),
        Record::new(
            "failed-with-quotes-and-backslashes",
            frame(&Notice::Failed { error: HOSTILE_ERROR.to_owned() }),
            "failed",
        )
        .with_error(HOSTILE_ERROR),
        Record::new(
            "failed-with-control-characters",
            frame(&Notice::Failed { error: "line one\nline two\ttabbed".to_owned() }),
            "failed",
        )
        .with_error("line one\nline two\ttabbed"),
    ];

    // Frames this server does not emit today, kept because the firmware must
    // not be broken by them: a member it does not know, whitespace a proxy
    // added, or a type from a newer server.
    records.extend([
        Record::new(
            "done-with-an-unknown-member",
            r#"{"type":"done","turns":2,"detail":{"nested":[1,"two"]}}"#.to_owned(),
            "done",
        ),
        Record::new(
            "started-with-whitespace",
            format!("{{ \"type\" : \"started\" , \"conversation\" : \"{CONVERSATION}\" }}"),
            "started",
        )
        .with_conversation(CONVERSATION),
        Record::new(
            "a-type-this-firmware-predates",
            r#"{"type":"paused"}"#.to_owned(),
            "unknown",
        ),
        Record::new("not-an-object", r#"["done"]"#.to_owned(), "unknown"),
    ]);

    records
}

/// Escapes a field for the fixture's tab-separated lines.
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Renders the fixture the firmware test suite reads.
fn render() -> String {
    let mut fixture = String::new();
    fixture.push_str("conduit-notice-fixture 1\n");
    fixture.push_str(
        "# Generated from crates/conduit-core/src/device.rs by\n\
         # `cargo test -p conduit-api --test protocol_parity`. Do not edit by hand:\n\
         # that test regenerates this file and fails when the two differ.\n\
         #\n\
         # One record per line, tab separated:\n\
         #   name <TAB> frame <TAB> expected type <TAB> field=value ...\n\
         # Fields escape a backslash as \\\\, a tab as \\t, a newline as \\n, and a\n\
         # carriage return as \\r. Nothing else is escaped.\n",
    );

    for record in records() {
        let mut line =
            format!("{}\t{}\t{}", record.name, escape(&record.frame), record.expect_type);
        if let Some(conversation) = &record.conversation {
            write!(line, "\tconversation={}", escape(conversation)).expect("a string grows");
        }
        if let Some(error) = &record.error {
            write!(line, "\terror={}", escape(error)).expect("a string grows");
        }
        fixture.push_str(&line);
        fixture.push('\n');
    }

    fixture
}

/// The value of a `static constexpr` in the firmware header, as written.
fn firmware_constant(header: &str, name: &str) -> String {
    let needle = format!("{name} = ");
    let (_, rest) = header
        .split_once(&needle)
        .unwrap_or_else(|| panic!("the firmware header must define {name}"));
    let (value, _) = rest.split_once(';').unwrap_or_else(|| panic!("{name} must end in `;`"));
    value.trim().to_owned()
}

/// The value of a firmware string constant, as Rust sees it.
///
/// The header writes C++ string literals, so the escaping is the same as JSON's
/// for everything these constants contain.
fn firmware_string(header: &str, name: &str) -> String {
    let literal = firmware_constant(header, name);
    serde_json::from_str::<String>(&literal)
        .unwrap_or_else(|error| panic!("{name} is not a plain string literal: {error}"))
}

#[test]
fn the_checked_in_fixture_matches_what_rust_generates() {
    let path = fixture_path();
    let generated = render();

    if std::env::var_os(REGENERATE).is_some() {
        std::fs::write(&path, &generated)
            .unwrap_or_else(|error| panic!("write {path:?}: {error}"));
        return;
    }

    let checked_in =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    assert_eq!(
        checked_in, generated,
        "{path:?} is stale or was edited by hand; regenerate it with \
         `{REGENERATE}=1 cargo test -p conduit-api --test protocol_parity`"
    );
}

#[test]
fn the_firmware_sends_the_end_command_this_server_parses() {
    let header = firmware_header();
    assert_eq!(
        firmware_string(&header, "CONDUIT_VOICE_CONVERSE_END_JSON"),
        serde_json::to_string(&Command::End).expect("serializes"),
        "the firmware's end-of-utterance frame must be what `Command` parses"
    );
}

#[test]
fn the_firmware_sends_the_stop_command_this_server_parses() {
    let header = firmware_header();
    assert_eq!(
        firmware_string(&header, "CONDUIT_VOICE_CONVERSE_STOP_JSON"),
        serde_json::to_string(&Command::Stop).expect("serializes"),
        "a device would ask for a stop this server ignores as an unknown command"
    );
}

#[test]
fn the_firmware_builds_the_route_this_server_serves() {
    let header = firmware_header();
    let prefix = firmware_string(&header, "CONDUIT_VOICE_CONVERSE_PATH_PREFIX");
    let suffix = firmware_string(&header, "CONDUIT_VOICE_CONVERSE_PATH_SUFFIX");

    assert_eq!(
        format!("{prefix}{{name}}{suffix}"),
        conduit_api::CONVERSE_ROUTE,
        "a device would open a path this server does not route"
    );
}

#[test]
fn the_firmware_captures_the_audio_format_the_pipeline_expects() {
    let header = firmware_header();
    let format = AudioFormat::DEFAULT;

    assert_eq!(
        firmware_constant(&header, "CONDUIT_VOICE_AUDIO_SAMPLE_RATE_HZ"),
        format.sample_rate.to_string(),
    );
    assert_eq!(
        firmware_constant(&header, "CONDUIT_VOICE_AUDIO_CHANNELS"),
        format.channels.to_string(),
    );
    // The encoding is not a number, so it is checked by the width it implies:
    // the pipeline's interchange format is signed 16-bit little-endian PCM.
    assert_eq!(firmware_constant(&header, "CONDUIT_VOICE_AUDIO_BITS_PER_SAMPLE"), "16");
    assert_eq!(
        format.encoding,
        conduit_core::audio::Encoding::PcmS16Le,
        "the firmware's 16-bit assumption is only sound for s16le audio"
    );
}
