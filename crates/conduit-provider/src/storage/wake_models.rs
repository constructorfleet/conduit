//! Resolving a wake phrase to the model a satellite has to flash.
//!
//! A device-runtime microWakeWord definition names *phrases*; firmware needs
//! *models*. This module is the mapping between them, and it exists here rather
//! than in the renderer because it is a property of the engine's vocabulary, not
//! of ESPHome YAML.
//!
//! [ADR-0015][adr] decided the shape: a table of phrases microWakeWord's own
//! manifest knows, plus an explicit-URL escape hatch on the definition, because
//! the alternative — Conduit hosting model files — is a different project. The
//! table is the ongoing cost of that decision.
//!
//! **An unresolvable phrase is an error, never an omission.** A device flashed
//! without the model for a phrase the server believes it detects is exactly the
//! silent disagreement rendering exists to prevent, so resolution refuses rather
//! than quietly rendering a shorter list.
//!
//! [adr]: ../../../../../docs/adr/0015-render-the-conduit-part-of-the-firmware.md

use super::wake::{MicroWakeWordRuntime, WakeVariant};

/// Phrases microWakeWord's bundled manifest resolves by name.
///
/// ESPHome accepts `model: hey_jarvis` and finds the manifest itself, so these
/// need no URL. Kept sorted, and matched case-insensitively after normalising
/// separators, because an operator types "hey jarvis" and a manifest spells it
/// `hey_jarvis`.
const MANIFEST_PHRASES: &[&str] = &["alexa", "hey_jarvis", "hey_mycroft", "okay_nabu", "stop"];

/// Phrases that end a reply rather than beginning a turn.
///
/// microWakeWord scores these like any other model, but they are not wake
/// words: nobody wants a "Stop" toggle in their smart-home app beside the
/// wake words, so both board files mark this one `internal: true`. Rendering
/// it as an ordinary model would publish an entity the hand-written files
/// deliberately hid.
const STOP_PHRASES: &[&str] = &["stop"];

/// Where a rendered `micro_wake_word:` entry gets its model file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelReference {
    /// A name ESPHome resolves from microWakeWord's own manifest.
    Manifest(String),
    /// An explicit `.json` manifest URL, emitted verbatim.
    Url(String),
}

impl ModelReference {
    /// The value to render after `model:`.
    ///
    /// Both arms render as their inner string; the variants exist so a caller
    /// can tell a manifest name from a URL without parsing one.
    #[must_use]
    pub fn rendered(&self) -> &str {
        match self {
            Self::Manifest(name) => name,
            Self::Url(url) => url,
        }
    }
}

/// One entry in a rendered `micro_wake_word:` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeModel {
    /// Where the model file comes from.
    pub reference: ModelReference,
    /// The ESPHome id for this model.
    ///
    /// Always the manifest spelling of the phrase, even when the model itself
    /// is an explicit URL — that is what both board files do, and an id is a
    /// handle for the rest of the document rather than a description of the
    /// file it loads.
    pub id: String,
    /// Whether ESPHome should keep this model's switch off the entity list.
    pub internal: bool,
}

impl WakeModel {
    /// The value to render after `model:`.
    #[must_use]
    pub fn rendered(&self) -> &str {
        self.reference.rendered()
    }
}

/// Why a phrase could not become a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPhrase {
    /// The phrase the definition named.
    pub phrase: String,
}

impl std::fmt::Display for UnknownPhrase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "no microWakeWord model is known for the phrase `{}`; add its manifest URL to the \
             definition's `models` map",
            self.phrase
        )
    }
}

impl std::error::Error for UnknownPhrase {}

/// The models a satellite running `variant` has to carry.
///
/// Returns an empty list for anything scored on a server: a Wyoming runtime's
/// phrases are scored off the device, so they belong in no firmware. That is a
/// correct empty answer rather than a failure, and the renderer relies on the
/// distinction to emit no `micro_wake_word:` block at all.
///
/// # Errors
///
/// Returns [`UnknownPhrase`] for the first phrase with neither an explicit URL
/// on the definition nor an entry in the manifest table.
pub fn models_for(variant: &WakeVariant) -> Result<Vec<WakeModel>, UnknownPhrase> {
    let WakeVariant::MicroWakeWord { runtime: MicroWakeWordRuntime::Device, phrases, .. } =
        variant
    else {
        return Ok(Vec::new());
    };

    phrases
        .iter()
        .map(|phrase| {
            let id = identifier(phrase);
            // The definition wins: an operator who named a URL for a phrase the
            // table also knows meant their model, not ours. An unknown phrase
            // is only unknown when nothing named a file for it, so a URL alone
            // is enough — that is what the escape hatch is for.
            let reference = match variant.model_url(phrase) {
                Some(url) => ModelReference::Url(url.to_owned()),
                None => ModelReference::Manifest(
                    manifest_name(phrase)
                        .ok_or_else(|| UnknownPhrase { phrase: phrase.clone() })?,
                ),
            };

            Ok(WakeModel { reference, internal: is_stop_phrase(&id), id })
        })
        .collect()
}

/// The ESPHome id for `phrase`: its normalised spelling.
fn identifier(phrase: &str) -> String {
    phrase.trim().to_lowercase().replace([' ', '-'], "_")
}

/// The manifest spelling of `phrase`, if microWakeWord ships one.
fn manifest_name(phrase: &str) -> Option<String> {
    let normalised = identifier(phrase);
    MANIFEST_PHRASES
        .iter()
        .find(|known| **known == normalised.as_str())
        .map(|known| (*known).to_owned())
}

/// Whether `id` names a phrase that stops a reply rather than starting a turn.
fn is_stop_phrase(id: &str) -> bool {
    STOP_PHRASES.contains(&id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn on_device(phrases: &[&str], models: &[(&str, &str)]) -> WakeVariant {
        WakeVariant::MicroWakeWord {
            runtime: MicroWakeWordRuntime::Device,
            phrases: phrases.iter().map(|phrase| (*phrase).to_owned()).collect(),
            models: models
                .iter()
                .map(|(phrase, url)| ((*phrase).to_owned(), (*url).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn a_phrase_the_manifest_knows_renders_as_its_bare_name() {
        // ESPHome resolves `model: hey_jarvis` itself, so emitting a URL here
        // would pin a copy of a file upstream already ships.
        let models = models_for(&on_device(&["hey_jarvis"], &[])).expect("a known phrase");

        assert_eq!(models[0].reference, ModelReference::Manifest("hey_jarvis".to_owned()));
        assert_eq!(models[0].rendered(), "hey_jarvis");
        assert_eq!(models[0].id, "hey_jarvis");
        assert!(!models[0].internal, "a wake word belongs on the entity list");
    }

    #[test]
    fn an_operator_spelling_a_phrase_as_words_still_resolves() {
        // Operators type what they say; manifests use identifiers. Refusing
        // "hey jarvis" would be refusing the spelling the console shows.
        for spelling in ["hey jarvis", "Hey Jarvis", "hey-jarvis", "  hey_jarvis  "] {
            let models = models_for(&on_device(&[spelling], &[]))
                .unwrap_or_else(|_| panic!("`{spelling}` names a known model"));
            assert_eq!(models[0].rendered(), "hey_jarvis", "spelling `{spelling}`");
        }
    }

    #[test]
    fn an_explicit_url_is_emitted_verbatim() {
        let url =
            "https://fph-firmware-assets.s3.us-east-1.amazonaws.com/wake-word/custom.json";
        let models = models_for(&on_device(&["custom"], &[("custom", url)])).expect("a url");

        assert_eq!(models[0].reference, ModelReference::Url(url.to_owned()));
        assert_eq!(models[0].rendered(), url);
    }

    #[test]
    fn a_stop_phrase_is_internal_so_no_switch_appears_for_it() {
        // Both board files hide this one, because a "Stop" toggle beside the
        // wake words in a smart-home app is not something anybody asked for.
        let models = models_for(&on_device(&["stop"], &[])).expect("a known phrase");

        assert!(models[0].internal, "a stop word is not a wake word");
        assert_eq!(models[0].id, "stop");
    }

    #[test]
    fn a_phrase_with_only_a_url_still_gets_an_id_from_its_spelling() {
        // The id is a handle for the rest of the document, so it comes from the
        // phrase rather than from the file name in the URL — which is what both
        // board files do for their S3 and GitHub models.
        let url = "https://example.invalid/whatever-the-file-is-called.json";
        let models = models_for(&on_device(&["Okay Nabu"], &[("Okay Nabu", url)]))
            .expect("a url needs no table entry");

        assert_eq!(models[0].id, "okay_nabu");
        assert_eq!(models[0].rendered(), url);
    }

    #[test]
    fn a_url_for_a_phrase_the_table_never_heard_of_resolves() {
        // The escape hatch has to actually be an escape hatch: naming a file is
        // enough, or a household could only flash the five phrases we listed.
        let url = "https://example.invalid/pod_bay_doors.json";
        let models = models_for(&on_device(&["pod bay doors"], &[("pod bay doors", url)]))
            .expect("a named file is enough");

        assert_eq!(models[0].rendered(), url);
        assert_eq!(models[0].id, "pod_bay_doors");
    }

    #[test]
    fn an_explicit_url_beats_the_table() {
        // A household serving its own build of a stock phrase gets its build.
        let url = "https://example.invalid/my_hey_jarvis.json";
        let models =
            models_for(&on_device(&["hey_jarvis"], &[("hey_jarvis", url)])).expect("a url");

        assert_eq!(models[0].rendered(), url, "the definition wins over the table");
    }

    #[test]
    fn a_phrase_with_no_model_anywhere_is_refused_by_name() {
        // The whole point: rendering a shorter model list would flash a device
        // that cannot hear a phrase the server thinks it detects.
        let error = models_for(&on_device(&["open the pod bay doors"], &[]))
            .expect_err("an unknown phrase cannot render");

        assert_eq!(error.phrase, "open the pod bay doors");
        let message = error.to_string();
        assert!(message.contains("open the pod bay doors"), "names the phrase: {message}");
        assert!(message.contains("`models`"), "says how to fix it: {message}");
    }

    #[test]
    fn a_definition_scored_on_a_server_carries_no_models() {
        // Not an error and not an oversight: a Wyoming server scores these, so
        // no firmware needs them. The renderer emits no block at all.
        let variant = WakeVariant::MicroWakeWord {
            runtime: MicroWakeWordRuntime::Wyoming {
                url: "tcp://microwakeword:10400".to_owned(),
                threshold_percent: 70,
            },
            phrases: vec!["something upstream has never heard of".to_owned()],
            models: BTreeMap::new(),
        };

        assert_eq!(models_for(&variant).expect("a server runtime resolves"), Vec::new());
    }

    #[test]
    fn phrases_resolve_in_the_order_the_definition_names_them() {
        // A rendered fragment has to be byte-identical between runs, or every
        // re-render looks like a change worth flashing.
        let models = models_for(&on_device(&["okay_nabu", "hey_jarvis", "stop"], &[]))
            .expect("all known");

        let rendered: Vec<&str> = models.iter().map(WakeModel::rendered).collect();
        assert_eq!(rendered, vec!["okay_nabu", "hey_jarvis", "stop"]);
    }
}
