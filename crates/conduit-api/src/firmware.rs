//! Rendering the Conduit part of a satellite's ESPHome configuration.
//!
//! Conduit renders the two blocks that describe **what a device talks to and
//! what it listens for** — `conduit_voice:` and `micro_wake_word:` — as a
//! fragment a hand-written board file includes. It renders nothing that
//! describes what a device is made of. That split is
//! [ADR-0015](../../../docs/adr/0015-render-the-conduit-part-of-the-firmware.md)
//! decision one, and it is what lets a board Conduit has never heard of work:
//! the fragment refers to the board only through IDs the board declares.
//!
//! **Every rendered field is an interpolation into a config format**, so every
//! one is validated before emission rather than trusting ESPHome to catch it.
//! The component's own schema is the authority — `_validate_pipeline` bounds
//! length and restricts the character set, `_validate_token` rejects CR and LF
//! because the token is interpolated into a raw header block — and a value that
//! reaches a device is one that got past both.
//!
//! **No rendered secret is ever a rendered value.** `token:` and
//! `debug_wake_event_url:` are emitted as `!secret` references, never as the
//! credential, so the fragment is safe to commit and useless to whoever holds
//! it. This handler reads no token from storage in order to render one; there is
//! no code path from a stored credential to rendered output.

use axum::extract::{Path, Query, State};
use conduit_core::graph::{Node, PipelineGraph};
use conduit_provider::storage::wake_models::{models_for, WakeModel};
use conduit_provider::storage::ProviderDefinitionVariant;
use serde::Deserialize;

use crate::auth::{Access, ManagementCaller};
use crate::pipelines::store_failure;
use crate::{ApiError, AppState};

/// Longest a rendered identifier may be.
///
/// Mirrors `_validate_pipeline`'s bound in the component, because the component
/// is what will refuse it otherwise, and refusing here names the field.
const MAXIMUM_NAME_LENGTH: usize = 128;

/// The `!secret` key the device token is read from.
///
/// The name the board files already use, so a rendered fragment drops into an
/// existing `secrets.yaml` without renaming anything.
const TOKEN_SECRET: &str = "conduit_token";

/// The `!secret` key the wake debug webhook URL is read from.
///
/// A Home Assistant webhook URL carries its token in the path, so the URL *is*
/// the credential and is never rendered as a value.
const WAKE_EVENT_URL_SECRET: &str = "wake_debug_event_url";

/// Largest microphone gain a rendered fragment will ask for.
///
/// The two boards use 6 and 4. An unbounded multiplier is a way to render a
/// configuration that clips every sample, which reads as a broken microphone
/// rather than as a bad parameter.
const MAXIMUM_GAIN_FACTOR: u8 = 32;

/// Longest utterance a rendered fragment will cap at, in milliseconds.
///
/// Ten minutes. The component defaults to eight seconds; the bound exists so
/// the value is a cap rather than an accident.
const MAXIMUM_UTTERANCE_MS: u32 = 600_000;

/// What a board declares, and what a device dials.
///
/// Every field here is either a board property or a connection property, and
/// none is derivable from a pipeline: ADR-0015 dissolves the "board profile"
/// question by observing that the board file *is* the profile, and that Conduit
/// needs to know only the handful of IDs the fragment refers to it by.
#[derive(Debug, Clone, Deserialize)]
pub struct RenderRequest {
    /// The pipeline this device converses with.
    pipeline: String,
    /// The board's microphone ID.
    microphone: String,
    /// The board's speaker ID.
    speaker: String,
    /// The board's mute switch ID, checked before a wake activation starts a
    /// turn.
    mute_switch: String,
    /// Microphone gain for wake word scoring. A microphone property, not a
    /// pipeline property — 6 on Satellite1, 4 on Voice PE.
    gain_factor: u8,
    /// Host and port the device dials, as `host:port`.
    server: String,
    /// `ws` or `wss`.
    #[serde(default = "default_scheme")]
    scheme: String,
    /// Longest utterance the device streams before it stops on its own.
    #[serde(default = "default_max_utterance_ms")]
    max_utterance_ms: u32,
    /// UDP host for wake debug audio. Empty disables it, as in both boards.
    #[serde(default)]
    debug_udp_host: String,
    /// UDP port for wake debug audio.
    #[serde(default = "default_debug_udp_port")]
    debug_udp_port: u16,
}

fn default_scheme() -> String {
    "ws".to_owned()
}

const fn default_max_utterance_ms() -> u32 {
    8_000
}

const fn default_debug_udp_port() -> u16 {
    6056
}

/// `GET /v1/devices/{device}/firmware` — the Conduit part of a device's ESPHome
/// configuration.
///
/// Behind a management token, emphatically not a device token: the fragment
/// describes a pipeline's wake configuration, and `auth.rs` already treats the
/// two audiences as a hard boundary. Extracting [`ManagementCaller`] is what
/// enforces that, so there is no check to write here.
///
/// The `device` in the path is the name from the token file, which is the one
/// device identifier that survives a restart — `DeviceId` is minted per process.
/// Keying on it is what makes a rendered fragment reproducible.
///
/// # Errors
///
/// Returns 404 if no device of that name is declared or the pipeline does not
/// exist, 422 if a parameter is unusable, if the device may not open that
/// pipeline, or if a wake phrase has no known model, and 503 if the store is
/// unavailable.
pub async fn render(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    Path(device): Path<String>,
    Query(request): Query<RenderRequest>,
) -> Result<([(axum::http::header::HeaderName, &'static str); 1], String), ApiError> {
    let request = request.validated()?;
    let device = resolve_device(state.access(), &device)?;

    if !device.may_use(&request.pipeline) {
        // Rendering a fragment naming a pipeline this device will be refused
        // when it connects is rendering a device that cannot work.
        return Err(ApiError::unprocessable(format!(
            "device `{}` may not open pipeline `{}`, so a fragment naming it would flash a \
             device that cannot converse",
            device.name, request.pipeline
        )));
    }

    let graph =
        state.pipeline(&request.pipeline).await.map_err(store_failure)?.ok_or_else(|| {
            ApiError::not_found(format!("no pipeline `{}`", request.pipeline))
        })?;

    let models = wake_models(&state, &graph).await?;
    let fragment = render_fragment(&request, &models);

    Ok(([(axum::http::header::CONTENT_TYPE, "application/yaml; charset=utf-8")], fragment))
}

/// The device this fragment is for.
///
/// On a server with a token file, the name has to be declared: rendering for a
/// device nobody configured is rendering for a device that does not exist. On an
/// open server there are no declared names, so the path name is a label — and
/// because every credential is rendered as a `!secret` reference, the fragment
/// is identical whatever it is labelled.
fn resolve_device(access: &Access, name: &str) -> Result<crate::auth::Device, ApiError> {
    access
        .device_named(name)
        .ok_or_else(|| ApiError::not_found(format!("no device `{name}` in the token file")))
}

/// The models the satellite this pipeline serves has to carry.
///
/// A pipeline with no wake stage, or one whose detector is scored on a server,
/// yields none — and that is a correct empty answer rather than a failure, so
/// the fragment simply carries no `micro_wake_word:` block.
async fn wake_models(
    state: &AppState,
    graph: &PipelineGraph,
) -> Result<Vec<WakeModel>, ApiError> {
    let Some(provider) = graph.nodes.iter().find_map(|node| match node {
        Node::WakeWord { provider, .. } => Some(provider),
        _ => None,
    }) else {
        return Ok(Vec::new());
    };

    // A stage naming a definition that is not stored is a pipeline that cannot
    // resolve at all; reporting it here beats rendering a device with no models
    // and letting the operator find out at a flash.
    let definition =
        state.provider_definition(provider).await.map_err(store_failure)?.ok_or_else(|| {
            ApiError::unprocessable(format!(
                "the pipeline's wake stage names provider definition `{provider}`, which does \
                 not exist"
            ))
        })?;

    let ProviderDefinitionVariant::Wake { variant } = &definition.variant else {
        return Err(ApiError::unprocessable(format!(
            "the pipeline's wake stage names provider definition `{provider}`, which is not a \
             wake word detector"
        )));
    };

    models_for(variant).map_err(|unknown| ApiError::unprocessable(unknown.to_string()))
}

impl RenderRequest {
    /// Checks every field that will be interpolated into YAML.
    ///
    /// The component validates these too, and that is the point: a value that
    /// reaches a device got past both. Refusing here names the field, which a
    /// failed ESPHome compile does not.
    fn validated(self) -> Result<Self, ApiError> {
        identifier("pipeline", &self.pipeline)?;
        identifier("microphone", &self.microphone)?;
        identifier("speaker", &self.speaker)?;
        identifier("mute_switch", &self.mute_switch)?;
        server("server", &self.server)?;

        if self.scheme != "ws" && self.scheme != "wss" {
            return Err(ApiError::unprocessable(format!(
                "scheme must be `ws` or `wss`, not `{}`",
                self.scheme
            )));
        }
        if self.gain_factor == 0 || self.gain_factor > MAXIMUM_GAIN_FACTOR {
            return Err(ApiError::unprocessable(format!(
                "gain_factor must be between 1 and {MAXIMUM_GAIN_FACTOR}"
            )));
        }
        if self.max_utterance_ms > MAXIMUM_UTTERANCE_MS {
            return Err(ApiError::unprocessable(format!(
                "max_utterance_ms must be {MAXIMUM_UTTERANCE_MS} or fewer"
            )));
        }
        // An empty host is how both boards disable UDP debug, so it is allowed;
        // a non-empty one is interpolated and has to be a host.
        if !self.debug_udp_host.is_empty() {
            server("debug_udp_host", &self.debug_udp_host)?;
        }

        Ok(self)
    }
}

/// Refuses anything that is not an ESPHome identifier.
///
/// Mirrors the component's `_validate_pipeline`: non-empty, bounded, and letters,
/// digits, `-` and `_` only. That character set is what makes interpolation safe
/// — nothing in it can close a YAML scalar, start a comment, or introduce a key.
fn identifier(field: &str, value: &str) -> Result<(), ApiError> {
    if value.is_empty() {
        return Err(ApiError::unprocessable(format!("{field} must not be empty")));
    }
    if value.len() > MAXIMUM_NAME_LENGTH {
        return Err(ApiError::unprocessable(format!(
            "{field} must be {MAXIMUM_NAME_LENGTH} characters or fewer"
        )));
    }
    if let Some(bad) =
        value.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(ApiError::unprocessable(format!(
            "{field} may contain only letters, numbers, `-` and `_`, not `{bad}`"
        )));
    }
    Ok(())
}

/// Refuses a host that is not a bare `host` or `host:port`.
///
/// Deliberately narrow. A scheme belongs in `scheme`, and anything carrying a
/// space, a newline, or YAML punctuation is a value that would change the shape
/// of the document rather than fill in a field.
fn server(field: &str, value: &str) -> Result<(), ApiError> {
    if value.is_empty() {
        return Err(ApiError::unprocessable(format!("{field} must not be empty")));
    }
    if value.len() > MAXIMUM_NAME_LENGTH {
        return Err(ApiError::unprocessable(format!(
            "{field} must be {MAXIMUM_NAME_LENGTH} characters or fewer"
        )));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':');
    if let Some(bad) = value.chars().find(|c| !allowed(*c)) {
        return Err(ApiError::unprocessable(format!(
            "{field} must be `host` or `host:port`; `{bad}` is not allowed"
        )));
    }
    Ok(())
}

/// Builds the fragment.
///
/// Takes validated inputs, so every interpolation here is of a value already
/// known to be an identifier, a host, or a bounded number. The two credentials
/// are `!secret` references and are not inputs at all.
fn render_fragment(request: &RenderRequest, models: &[WakeModel]) -> String {
    let mut yaml = String::new();
    yaml.push_str(
        "# Rendered by Conduit. Edits are lost on the next render.\n\
         #\n\
         # Included by a hand-written board file, which owns everything about what\n\
         # the board is made of. See docs/adr/0015-render-the-conduit-part-of-the-firmware.md\n\
         #\n\
         # Credentials are `!secret` references, never values, so this file is safe\n\
         # to commit. Define them in your ESPHome `secrets.yaml`.\n\n",
    );

    if !models.is_empty() {
        yaml.push_str("micro_wake_word:\n  id: mww\n  microphone:\n");
        yaml.push_str(&format!("    microphone: {}\n", request.microphone));
        yaml.push_str("    channels: 1\n");
        yaml.push_str(&format!("    gain_factor: {}\n", request.gain_factor));
        yaml.push_str("  stop_after_detection: false\n  vad:\n  models:\n");
        for model in models {
            yaml.push_str(&format!("    - model: {}\n", model.rendered()));
            yaml.push_str(&format!("      id: {}\n", model.id));
            // Emitted only when true, matching the board files: an explicit
            // `internal: false` on every wake word would be noise.
            if model.internal {
                yaml.push_str("      internal: true\n");
            }
        }
        yaml.push_str(&format!(
            "  on_wake_word_detected:\n\
             \x20   - if:\n\
             \x20       condition:\n\
             \x20         switch.is_off: {}\n\
             \x20       then:\n\
             \x20         - conduit_voice.wake_debug_event:\n\
             \x20             id: conduit\n\
             \x20         - conduit_voice.start:\n\
             \x20             id: conduit\n\n",
            request.mute_switch
        ));
    }

    yaml.push_str("conduit_voice:\n  id: conduit\n");
    yaml.push_str(&format!("  server: {}\n", request.server));
    yaml.push_str(&format!("  scheme: {}\n", request.scheme));
    yaml.push_str(&format!("  pipeline: {}\n", request.pipeline));
    yaml.push_str("  # A reference, never a value: this file is committed.\n");
    yaml.push_str(&format!("  token: !secret {TOKEN_SECRET}\n"));
    yaml.push_str(&format!("  max_utterance_ms: {}\n", request.max_utterance_ms));
    yaml.push_str(&format!("  debug_assistant_id: {}\n", request.pipeline));
    yaml.push_str(&format!("  debug_udp_host: \"{}\"\n", request.debug_udp_host));
    yaml.push_str(&format!("  debug_udp_port: {}\n", request.debug_udp_port));
    yaml.push_str("  # A reference, never a value: the URL carries a webhook token.\n");
    yaml.push_str(&format!("  debug_wake_event_url: !secret {WAKE_EVENT_URL_SECRET}\n"));
    yaml.push_str(&format!("  microphone: {}\n", request.microphone));
    yaml.push_str(&format!("  speaker: {}\n", request.speaker));

    yaml
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_provider::storage::wake_models::ModelReference;

    fn request() -> RenderRequest {
        RenderRequest {
            pipeline: "kitchen".to_owned(),
            microphone: "sat1_mics".to_owned(),
            speaker: "announcement_resampling_speaker".to_owned(),
            mute_switch: "master_mute_switch".to_owned(),
            gain_factor: 6,
            server: "192.168.1.10:8080".to_owned(),
            scheme: "ws".to_owned(),
            max_utterance_ms: 8_000,
            debug_udp_host: String::new(),
            debug_udp_port: 6056,
        }
    }

    /// A wake word resolved from microWakeWord's manifest.
    fn manifest(phrase: &str) -> WakeModel {
        WakeModel {
            reference: ModelReference::Manifest(phrase.to_owned()),
            id: phrase.to_owned(),
            internal: false,
        }
    }

    /// A wake word whose model file the definition named.
    fn url(id: &str, url: &str) -> WakeModel {
        WakeModel {
            reference: ModelReference::Url(url.to_owned()),
            id: id.to_owned(),
            internal: false,
        }
    }

    #[test]
    fn no_credential_is_ever_rendered_as_a_value() {
        // The security property, asserted as a property rather than as a
        // spot-check of one field: the renderer takes no credential as input, so
        // there is nothing it could leak, and the output says `!secret`.
        let yaml = render_fragment(&request(), &[manifest("hey_jarvis")]);

        assert!(yaml.contains("token: !secret conduit_token"));
        assert!(yaml.contains("debug_wake_event_url: !secret wake_debug_event_url"));
        for line in yaml.lines().filter(|line| {
            line.contains("token:") || line.contains("_url:") && !line.contains("udp")
        }) {
            assert!(
                line.contains("!secret"),
                "a credential-bearing field must be a reference: {line}"
            );
        }
    }

    #[test]
    fn a_pipeline_with_no_device_models_renders_no_wake_block() {
        // A Wyoming detector scores off the device, so the device has nothing to
        // flash and no block to carry — but it still needs to know what to dial.
        let yaml = render_fragment(&request(), &[]);

        assert!(!yaml.contains("micro_wake_word:"), "no models means no block: {yaml}");
        assert!(yaml.contains("conduit_voice:"), "the device still converses");
    }

    #[test]
    fn every_phrase_becomes_a_model_in_order() {
        let yaml = render_fragment(
            &request(),
            &[
                manifest("hey_jarvis"),
                url("okay_nabu", "https://example.invalid/okay_nabu.json"),
            ],
        );

        let models: Vec<&str> =
            yaml.lines().filter_map(|line| line.trim().strip_prefix("- model: ")).collect();
        assert_eq!(models, vec!["hey_jarvis", "https://example.invalid/okay_nabu.json"]);
    }

    #[test]
    fn a_model_carries_the_id_and_visibility_the_board_files_give_it() {
        // Both hand-written files give every model an `id:` and hide the stop
        // word. Rendering neither would publish a "Stop" switch the boards
        // deliberately suppressed, and leave the document with no handle for a
        // model — a regression the moment a board file switches to `!include`.
        let mut stop = manifest("stop");
        stop.internal = true;
        let yaml = render_fragment(&request(), &[manifest("hey_jarvis"), stop]);

        assert!(yaml.contains("    - model: hey_jarvis\n      id: hey_jarvis\n"), "{yaml}");
        assert!(
            yaml.contains("    - model: stop\n      id: stop\n      internal: true\n"),
            "{yaml}"
        );
        assert_eq!(yaml.matches("internal: true").count(), 1, "only the stop word is hidden");
    }

    #[test]
    fn the_fragment_refers_to_the_board_only_through_ids_it_was_given() {
        // The whole contract between rendered and hand-written parts. A board
        // Conduit has never heard of works because these are all it names.
        let yaml = render_fragment(&request(), &[manifest("stop")]);

        assert!(yaml.contains("microphone: sat1_mics"));
        assert!(yaml.contains("speaker: announcement_resampling_speaker"));
        assert!(yaml.contains("switch.is_off: master_mute_switch"));
        assert!(yaml.contains("gain_factor: 6"));
        // And nothing about what the board is made of.
        for hardware in ["spi:", "i2s_audio:", "audio_dac:", "cs_pin", "i2s_mclk_pin"] {
            assert!(!yaml.contains(hardware), "`{hardware}` is the board file's business");
        }
    }

    #[test]
    fn rendering_the_same_request_twice_is_byte_identical() {
        // A fragment that churned would make every re-render look like a change
        // worth flashing.
        let models = [manifest("okay_nabu")];
        assert_eq!(render_fragment(&request(), &models), render_fragment(&request(), &models));
    }

    #[test]
    fn a_field_that_would_change_the_shape_of_the_document_is_refused() {
        // Every rendered field is an injection site into a config format. These
        // are the shapes that matter: a newline introduces a key, a colon
        // introduces a mapping, a `#` comments out what follows.
        for bad in ["kitchen\ntoken: stolen", "kitchen: x", "kitchen #", "kitchen\r\n", ""] {
            let mut request = request();
            request.pipeline = bad.to_owned();
            assert!(
                request.validated().is_err(),
                "`{}` must be refused before it reaches YAML",
                bad.escape_debug()
            );
        }
    }

    #[test]
    fn every_interpolated_identifier_is_checked_not_just_the_pipeline() {
        // The component validates its pipeline name and nothing else, so an ID
        // is only checked here. Missing one would leave exactly one injection
        // site open.
        let fields: [fn(&mut RenderRequest); 3] = [
            |request| request.microphone = "mics\nspeaker: theirs".to_owned(),
            |request| request.speaker = "spk: x".to_owned(),
            |request| request.mute_switch = "sw #".to_owned(),
        ];
        for set in fields {
            let mut request = request();
            set(&mut request);
            assert!(request.validated().is_err(), "every rendered ID is validated");
        }
    }

    #[test]
    fn a_scheme_that_is_not_a_websocket_scheme_is_refused() {
        for bad in ["http", "https", "wsss", "WS "] {
            let mut request = request();
            request.scheme = bad.to_owned();
            assert!(request.validated().is_err(), "`{bad}` is not a websocket scheme");
        }
    }

    #[test]
    fn a_server_carrying_anything_but_a_host_and_port_is_refused() {
        for bad in ["ws://host:1", "host 1", "host\n", "host/path", ""] {
            let mut request = request();
            request.server = bad.to_owned();
            assert!(
                request.validated().is_err(),
                "`{}` is not a host:port",
                bad.escape_debug()
            );
        }
    }

    #[test]
    fn a_gain_or_utterance_cap_outside_its_bounds_is_refused() {
        let mut zero_gain = request();
        zero_gain.gain_factor = 0;
        assert!(zero_gain.validated().is_err(), "a gain of zero hears nothing");

        let mut loud = request();
        loud.gain_factor = MAXIMUM_GAIN_FACTOR + 1;
        assert!(loud.validated().is_err(), "an unbounded gain clips every sample");

        let mut forever = request();
        forever.max_utterance_ms = MAXIMUM_UTTERANCE_MS + 1;
        assert!(forever.validated().is_err(), "a cap has to cap something");
    }

    #[test]
    fn an_empty_debug_host_is_how_both_boards_disable_udp_debug() {
        let mut silent = request();
        silent.debug_udp_host = String::new();
        assert!(silent.validated().is_ok(), "empty disables it rather than being invalid");

        let mut bad = request();
        bad.debug_udp_host = "host\nkey: value".to_owned();
        assert!(bad.validated().is_err(), "a non-empty host is still interpolated");
    }

    #[test]
    fn the_two_boards_parameters_render_the_blocks_they_inline_today() {
        // Voice PE differs from Satellite1 in exactly the ways ADR-0015 says a
        // board differs: its own IDs and its own gain.
        let mut voicepe = request();
        voicepe.microphone = "i2s_mics".to_owned();
        voicepe.gain_factor = 4;

        let yaml = render_fragment(&voicepe, &[manifest("hey_jarvis")]);

        assert!(yaml.contains("microphone: i2s_mics"));
        assert!(yaml.contains("gain_factor: 4"));
        assert!(yaml.contains("pipeline: kitchen"));
        assert!(yaml.contains("debug_assistant_id: kitchen"));
    }
}
