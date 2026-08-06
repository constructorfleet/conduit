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

/// How a rendered `micro_wake_word:` entry names its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeModel {
    /// A name ESPHome resolves from microWakeWord's own manifest.
    Manifest(String),
    /// An explicit `.json` manifest URL, emitted verbatim.
    Url(String),
}

impl WakeModel {
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
            // The definition wins: an operator who named a URL for a phrase the
            // table also knows meant their model, not ours.
            if let Some(url) = variant.model_url(phrase) {
                return Ok(WakeModel::Url(url.to_owned()));
            }
            manifest_name(phrase)
                .map(WakeModel::Manifest)
                .ok_or_else(|| UnknownPhrase { phrase: phrase.clone() })
        })
        .collect()
}

/// The manifest spelling of `phrase`, if microWakeWord ships one.
fn manifest_name(phrase: &str) -> Option<String> {
    let normalised = phrase.trim().to_lowercase().replace([' ', '-'], "_");
    MANIFEST_PHRASES
        .iter()
        .find(|known| **known == normalised.as_str())
        .map(|known| (*known).to_owned())
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

        assert_eq!(models, vec![WakeModel::Manifest("hey_jarvis".to_owned())]);
        assert_eq!(models[0].rendered(), "hey_jarvis");
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

        assert_eq!(models, vec![WakeModel::Url(url.to_owned())]);
        assert_eq!(models[0].rendered(), url);
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
