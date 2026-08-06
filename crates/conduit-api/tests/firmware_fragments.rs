//! The rendered fragments the board files include, checked in.
//!
//! Each board's fragment is committed so the firmware suite has something to
//! grep without a Rust build first, and so a reviewer can read the renderer's
//! output rather than infer it. This test regenerates them and fails when the
//! checked-in copies differ, which is what stops one from being hand-edited —
//! the same mechanism `protocol_parity.rs` uses for `notices.fixture`.
//!
//! Driven through the real route rather than the renderer, because what the
//! board files include has to be what an operator actually downloads.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use conduit_api::{router, AppState};
use conduit_core::bus::EventBus;
use conduit_core::graph::{Edge, Modality, Node, PipelineGraph};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Environment variable that turns this test into a generator.
const REGENERATE: &str = "CONDUIT_REGENERATE_FIXTURES";

/// A board, and the parameters its hand-written file passes to the renderer.
///
/// These are exactly the values each board declared inline before it switched
/// to an `!include`, which is what makes the conversion checkable rather than
/// hopeful.
struct Board {
    /// The fragment's file name, beside the board file that includes it.
    fragment: &'static str,
    /// The pipeline the board's `substitutions:` block names.
    pipeline: &'static str,
    /// The board's microphone component id.
    microphone: &'static str,
    /// Microphone gain — the one number the two boards disagree on.
    gain_factor: u8,
    /// What the board's `substitutions:` block dials.
    server: &'static str,
    /// Where the board mirrors audio, empty when it does not.
    debug_udp_host: &'static str,
    /// The phrases that board flashes, in the order its file listed them.
    phrases: &'static [&'static str],
    /// Explicit model URLs, keyed by phrase, for the ones upstream does not
    /// resolve by name.
    models: &'static [(&'static str, &'static str)],
}

const SAT1_ASSETS: &str = "https://fph-firmware-assets.s3.us-east-1.amazonaws.com/wake-word";
const VOICEPE_RELEASES: &str = "https://github.com/kahrendt/microWakeWord/releases/download";

fn boards() -> Vec<Board> {
    vec![
        Board {
            fragment: "conduit-sat1.conduit.yaml",
            pipeline: "default",
            microphone: "sat1_mics",
            gain_factor: 6,
            server: "10.0.12.72:8080",
            debug_udp_host: "10.0.12.11",
            phrases: &["hey_jarvis", "okay_nabu", "stop"],
            // Satellite1 serves all three from its own bucket rather than
            // resolving any by name, so every one is an explicit URL.
            models: &[
                ("hey_jarvis", "{assets}/hey_jarvis.json"),
                ("okay_nabu", "{assets}/okay_nabu.json"),
                ("stop", "{assets}/stop.json"),
            ],
        },
        Board {
            fragment: "conduit-voicepe.conduit.yaml",
            pipeline: "kitchen",
            microphone: "i2s_mics",
            gain_factor: 4,
            server: "192.168.1.10:8080",
            debug_udp_host: "",
            phrases: &["okay_nabu", "hey_jarvis", "hey_mycroft", "stop"],
            // Voice PE pins two and lets ESPHome resolve the other two by
            // name, which is why both spellings have to keep working.
            models: &[
                ("okay_nabu", "{releases}/okay_nabu_20241226.3/okay_nabu.json"),
                ("stop", "{releases}/stop/stop.json"),
            ],
        },
    ]
}

/// Expands the two asset-host placeholders, which only exist to keep the table
/// above narrow enough to read.
fn expand(url: &str) -> String {
    url.replace("{assets}", SAT1_ASSETS).replace("{releases}", VOICEPE_RELEASES)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

/// The wake definition a board's phrases and model URLs describe.
fn wake_definition(board: &Board) -> serde_json::Value {
    let models: serde_json::Map<String, serde_json::Value> = board
        .models
        .iter()
        .map(|(phrase, url)| ((*phrase).to_owned(), expand(url).into()))
        .collect();

    serde_json::json!({
        "id": "satellite",
        "label": "Satellite",
        "variant": {
            "type": "wake",
            "variant": {
                "type": "microwakeword",
                "runtime": { "where": "device" },
                "phrases": board.phrases,
                "models": models,
            }
        }
    })
}

/// A voice pipeline that wakes on the device.
fn waking_graph(name: &str) -> PipelineGraph {
    PipelineGraph::new(name)
        .with_node(Node::source("source", "websocket", Modality::Audio))
        .with_node(Node::wake_word("wake", "satellite"))
        .with_node(Node::stt("stt", "whisper"))
        .with_node(Node::core("core", "ollama"))
        .with_node(Node::tts("tts", "piper"))
        .with_node(Node::sink("sink", "websocket", Modality::Audio))
        .with_edge(Edge::new("source", "wake"))
        .with_edge(Edge::new("wake", "stt"))
        .with_edge(Edge::new("stt", "core"))
        .with_edge(Edge::new("core", "tts"))
        .with_edge(Edge::new("tts", "sink"))
}

/// Renders `board`'s fragment through the real route.
async fn render(board: &Board) -> String {
    let state = AppState::new(EventBus::default());

    let stored = Request::builder()
        .method("PUT")
        .uri("/v1/providers/satellite")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&wake_definition(board)).expect("serialize")))
        .expect("request");
    let response = router(state.clone()).oneshot(stored).await.expect("router responds");
    assert!(response.status().is_success(), "storing the definition: {}", response.status());

    state
        .put_pipeline(board.pipeline, waking_graph(board.pipeline))
        .await
        .expect("stores the pipeline");

    let uri = format!(
        "/v1/devices/{name}/firmware?pipeline={pipeline}&microphone={microphone}\
         &speaker=announcement_resampling_speaker&mute_switch=master_mute_switch\
         &gain_factor={gain}&server={server}&debug_udp_host={debug_host}",
        name = board.pipeline,
        pipeline = board.pipeline,
        microphone = board.microphone,
        gain = board.gain_factor,
        server = board.server,
        debug_host = board.debug_udp_host,
    );
    let request = Request::builder().uri(uri).body(Body::empty()).expect("request");
    let response = router(state.clone()).oneshot(request).await.expect("router responds");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let yaml = String::from_utf8(bytes.to_vec()).expect("utf-8");
    assert_eq!(status, StatusCode::OK, "rendering {}: {yaml}", board.fragment);

    yaml
}

#[tokio::test]
async fn the_committed_fragments_are_what_the_renderer_produces() {
    let root = repo_root();
    let regenerate = std::env::var_os(REGENERATE).is_some();

    for board in boards() {
        let rendered = render(&board).await;
        let path = root.join("firmware/esphome").join(board.fragment);

        if regenerate {
            std::fs::write(&path, &rendered).unwrap_or_else(|error| {
                panic!("write {path:?}: {error}");
            });
            continue;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{} is missing or unreadable ({error}); run \
                 `{REGENERATE}=1 cargo test -p conduit-api --test firmware_fragments`",
                board.fragment
            )
        });
        assert_eq!(
            committed, rendered,
            "{} is stale; run `{REGENERATE}=1 cargo test -p conduit-api \
             --test firmware_fragments`",
            board.fragment
        );
    }
}

#[tokio::test]
async fn each_fragment_keeps_the_models_its_board_file_listed() {
    // The regression this whole track risks: a converted board file that flashes
    // a different set of models than the one it inlined. Asserted per board
    // rather than trusting the fixture diff, because a fixture regenerated in
    // the same commit that broke it would agree with itself.
    for board in boards() {
        let yaml = render(&board).await;

        for phrase in board.phrases {
            assert!(
                yaml.contains(&format!("id: {phrase}")),
                "{} lost `{phrase}`:\n{yaml}",
                board.fragment
            );
        }
        for (_, url) in board.models {
            assert!(
                yaml.contains(&expand(url)),
                "{} lost a pinned model:\n{yaml}",
                board.fragment
            );
        }
        // Every board hides its stop word and nothing else.
        assert_eq!(
            yaml.matches("internal: true").count(),
            1,
            "{} must hide exactly the stop word:\n{yaml}",
            board.fragment
        );
    }
}

#[tokio::test]
async fn no_committed_fragment_contains_a_credential() {
    // These files are committed, so this is the property that makes committing
    // them acceptable. Asserted against the files on disk rather than the
    // renderer: what a repository exposes is what is in the repository.
    let root = repo_root();

    for board in boards() {
        let path = root.join("firmware/esphome").join(board.fragment);
        let committed = std::fs::read_to_string(&path).expect("a committed fragment");

        for field in ["token", "debug_wake_event_url"] {
            let line = committed
                .lines()
                .find(|line| line.trim_start().starts_with(&format!("{field}:")))
                .unwrap_or_else(|| panic!("{} must set `{field}`", board.fragment));
            assert!(
                line.contains("!secret "),
                "{} renders `{field}` as a value: {line}",
                board.fragment
            );
        }
    }
}
