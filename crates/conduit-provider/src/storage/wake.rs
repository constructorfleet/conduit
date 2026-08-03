//! Wake word detection provider variants.

use serde::{Deserialize, Serialize};

use super::{default_threshold_percent, WakeEngine};

/// Where a detector Conduit can score itself is running.
///
/// openWakeWord and nanoWakeWord are ONNX end-to-end, so Conduit can either
/// load the models and score them in process or hand the audio to a Wyoming
/// server that does. Both are the same engine listening for the same phrases;
/// only the deployment differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "where", rename_all = "snake_case")]
pub enum WakeRuntime {
    /// Scored in the Conduit process from models on disk.
    Local {
        /// Directory the phrase models are read from. Unset means the
        /// conventional `wake-models` directory under the data directory,
        /// which is where an operator who followed the compose file put them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models_dir: Option<String>,
        /// Minimum confidence to accept, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    /// Scored by a Wyoming server.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Minimum confidence to accept, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
}

/// Where microWakeWord is running.
///
/// A separate type from [`WakeRuntime`] because microWakeWord's runtimes are a
/// different set, not a subset: its models are tflite-micro streaming graphs
/// that need the TFLM micro-frontend operator, which no Rust runtime
/// implements, so there is no local arm — and it is the only engine small
/// enough for satellite hardware, so it is the only one with a device arm.
/// Saying that in the type is what keeps `openwakeword` on a satellite, or
/// `microwakeword` in process, from being expressible at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "where", rename_all = "snake_case")]
pub enum MicroWakeWordRuntime {
    /// Scored on the satellite itself.
    ///
    /// There is no endpoint because there is no server: the device runs the
    /// detector and only streams audio once it has activated. It carries no
    /// threshold because there is nothing left to score — by the time the
    /// server sees a sample the decision has been made.
    Device,
    /// Scored by a Wyoming server.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Minimum confidence to accept, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
}

impl MicroWakeWordRuntime {
    /// The Wyoming endpoint, when detection happens on a server.
    #[must_use]
    pub fn wyoming_url(&self) -> Option<&str> {
        match self {
            Self::Device => None,
            Self::Wyoming { url, .. } => Some(url),
        }
    }

    /// The acceptance threshold, when there is something left to score.
    #[must_use]
    pub const fn threshold_percent(&self) -> Option<u8> {
        match self {
            Self::Device => None,
            Self::Wyoming { threshold_percent, .. } => Some(*threshold_percent),
        }
    }
}

impl WakeRuntime {
    /// The Wyoming endpoint, when detection happens on a server.
    #[must_use]
    pub fn wyoming_url(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Wyoming { url, .. } => Some(url),
        }
    }

    /// The directory phrase models are read from, when scoring in process.
    #[must_use]
    pub fn models_dir(&self) -> Option<Option<&str>> {
        match self {
            Self::Local { models_dir, .. } => Some(models_dir.as_deref()),
            Self::Wyoming { .. } => None,
        }
    }

    /// The acceptance threshold. Every runtime here scores, so there is always
    /// one.
    #[must_use]
    pub const fn threshold_percent(&self) -> u8 {
        match self {
            Self::Local { threshold_percent, .. } | Self::Wyoming { threshold_percent, .. } => {
                *threshold_percent
            }
        }
    }
}

/// Wake word detection provider variants, one per engine.
///
/// The engine is the variant rather than a field on a shared shape because the
/// three do not run in the same places and do not agree on what a phrase is —
/// a microWakeWord `.json` manifest, an openWakeWord model file, a
/// nanoWakeWord model. A definition that named an engine and a place
/// independently could describe combinations that do not exist, and the only
/// thing standing between an operator and one of those was a runtime check.
/// Now the combinations that exist are the ones the type can hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum WakeVariant {
    /// openWakeWord: the general-purpose detector Home Assistant ships.
    #[serde(rename = "openwakeword")]
    OpenWakeWord {
        /// Where detection happens.
        runtime: WakeRuntime,
        /// Phrases to listen for. Empty asks for whatever was loaded.
        #[serde(default)]
        phrases: Vec<String>,
    },
    /// nanoWakeWord: openWakeWord's lighter successor, same model vocabulary.
    #[serde(rename = "nanowakeword")]
    NanoWakeWord {
        /// Where detection happens.
        runtime: WakeRuntime,
        /// Phrases to listen for. Empty asks for whatever was loaded.
        #[serde(default)]
        phrases: Vec<String>,
    },
    /// microWakeWord: small models built for microcontrollers, which is why it
    /// is the engine an ESP32 satellite runs on-device.
    #[serde(rename = "microwakeword")]
    MicroWakeWord {
        /// Where detection happens.
        runtime: MicroWakeWordRuntime,
        /// Phrases to listen for. On a satellite the server never scores
        /// these; they are what an operator flashed, for operator screens.
        #[serde(default)]
        phrases: Vec<String>,
    },
}

impl WakeVariant {
    /// Which detector this definition describes.
    #[must_use]
    pub const fn engine(&self) -> WakeEngine {
        match self {
            Self::OpenWakeWord { .. } => WakeEngine::OpenWakeWord,
            Self::NanoWakeWord { .. } => WakeEngine::NanoWakeWord,
            Self::MicroWakeWord { .. } => WakeEngine::MicroWakeWord,
        }
    }

    /// The phrases the definition names.
    #[must_use]
    pub fn phrases(&self) -> &[String] {
        match self {
            Self::OpenWakeWord { phrases, .. }
            | Self::NanoWakeWord { phrases, .. }
            | Self::MicroWakeWord { phrases, .. } => phrases,
        }
    }

    /// The Wyoming endpoint, when detection happens on a server.
    #[must_use]
    pub fn wyoming_url(&self) -> Option<&str> {
        match self {
            Self::OpenWakeWord { runtime, .. } | Self::NanoWakeWord { runtime, .. } => {
                runtime.wyoming_url()
            }
            Self::MicroWakeWord { runtime, .. } => runtime.wyoming_url(),
        }
    }

    /// The acceptance threshold, when there is something left to score.
    #[must_use]
    pub const fn threshold_percent(&self) -> Option<u8> {
        match self {
            Self::OpenWakeWord { runtime, .. } | Self::NanoWakeWord { runtime, .. } => {
                Some(runtime.threshold_percent())
            }
            Self::MicroWakeWord { runtime, .. } => runtime.threshold_percent(),
        }
    }

    /// The directory phrase models are read from, when scoring in process.
    ///
    /// The outer [`Option`] is whether detection is local at all; the inner one
    /// is whether the definition named a directory or wants the conventional
    /// one.
    #[must_use]
    pub fn local_models_dir(&self) -> Option<Option<&str>> {
        match self {
            Self::OpenWakeWord { runtime, .. } | Self::NanoWakeWord { runtime, .. } => {
                runtime.models_dir()
            }
            Self::MicroWakeWord { .. } => None,
        }
    }

    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        self.clone()
    }

    /// Folds a record that named an engine and a Wyoming server as independent
    /// fields, from before the engine became the variant.
    pub(super) fn from_engine_on_wyoming(
        engine: WakeEngine,
        url: String,
        phrases: Vec<String>,
        threshold_percent: u8,
    ) -> Self {
        match engine {
            WakeEngine::OpenWakeWord => Self::OpenWakeWord {
                runtime: WakeRuntime::Wyoming { url, threshold_percent },
                phrases,
            },
            WakeEngine::NanoWakeWord => Self::NanoWakeWord {
                runtime: WakeRuntime::Wyoming { url, threshold_percent },
                phrases,
            },
            WakeEngine::MicroWakeWord => Self::MicroWakeWord {
                runtime: MicroWakeWordRuntime::Wyoming { url, threshold_percent },
                phrases,
            },
        }
    }

    /// Folds a record that named an engine detecting on the satellite itself.
    ///
    /// Only microWakeWord ever ran there, and validation refused the others —
    /// so a stored record naming one is hand-written. It cannot be refused
    /// here: this runs while the provider snapshot is being rebuilt, and a
    /// refusal would take startup down over a definition an operator can no
    /// longer reach to fix. It becomes that engine detecting locally, which is
    /// the nearest place it can actually run, and says so in the log.
    pub(super) fn from_engine_on_device(engine: WakeEngine, phrases: Vec<String>) -> Self {
        match engine {
            WakeEngine::MicroWakeWord => {
                Self::MicroWakeWord { runtime: MicroWakeWordRuntime::Device, phrases }
            }
            WakeEngine::OpenWakeWord | WakeEngine::NanoWakeWord => {
                tracing::warn!(
                    engine = engine.name(),
                    "a stored definition says this engine detects on the satellite, which it is \
                     too large to do; reading it as detecting in process instead"
                );
                let runtime = WakeRuntime::Local {
                    models_dir: None,
                    threshold_percent: default_threshold_percent(),
                };
                if engine == WakeEngine::OpenWakeWord {
                    Self::OpenWakeWord { runtime, phrases }
                } else {
                    Self::NanoWakeWord { runtime, phrases }
                }
            }
        }
    }
}

/// The shape wake definitions were written in before the engine became the
/// variant.
///
/// Storage outlives the rule that wrote it, for the same reason the flat
/// pre-nesting shape is still read: a stored definition describes a provider an
/// operator configured, and it must keep working. Reads accept both; writes
/// always emit the per-engine shape, so saving a definition upgrades its stored
/// record in place.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LegacyWakeVariant {
    Wyoming {
        url: String,
        engine: WakeEngine,
        #[serde(default)]
        phrases: Vec<String>,
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    Device {
        engine: WakeEngine,
        #[serde(default)]
        phrases: Vec<String>,
    },
}

impl From<LegacyWakeVariant> for WakeVariant {
    fn from(legacy: LegacyWakeVariant) -> Self {
        match legacy {
            LegacyWakeVariant::Wyoming { url, engine, phrases, threshold_percent } => {
                Self::from_engine_on_wyoming(engine, url, phrases, threshold_percent)
            }
            LegacyWakeVariant::Device { engine, phrases } => {
                Self::from_engine_on_device(engine, phrases)
            }
        }
    }
}

/// The per-engine shape as a wire-only mirror.
///
/// [`WakeVariant`] derives [`Serialize`] but implements [`Deserialize`] by hand
/// so that pre-split definitions keep reading; a mirror is how that
/// hand-written [`Deserialize`] reaches the derived shape without the two
/// implementations living in one type.
#[derive(Deserialize)]
#[serde(tag = "type")]
// The shared postfix is the engines' own names, which is the whole point: an
// operator reading a stored definition should see what they installed.
#[allow(clippy::enum_variant_names)]
enum WireWakeVariant {
    #[serde(rename = "openwakeword")]
    OpenWakeWord {
        runtime: WakeRuntime,
        #[serde(default)]
        phrases: Vec<String>,
    },
    #[serde(rename = "nanowakeword")]
    NanoWakeWord {
        runtime: WakeRuntime,
        #[serde(default)]
        phrases: Vec<String>,
    },
    #[serde(rename = "microwakeword")]
    MicroWakeWord {
        runtime: MicroWakeWordRuntime,
        #[serde(default)]
        phrases: Vec<String>,
    },
}

impl From<WireWakeVariant> for WakeVariant {
    fn from(wire: WireWakeVariant) -> Self {
        match wire {
            WireWakeVariant::OpenWakeWord { runtime, phrases } => {
                Self::OpenWakeWord { runtime, phrases }
            }
            WireWakeVariant::NanoWakeWord { runtime, phrases } => {
                Self::NanoWakeWord { runtime, phrases }
            }
            WireWakeVariant::MicroWakeWord { runtime, phrases } => {
                Self::MicroWakeWord { runtime, phrases }
            }
        }
    }
}

impl<'de> Deserialize<'de> for WakeVariant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match serde_json::from_value::<WireWakeVariant>(value.clone()) {
            Ok(wire) => Ok(wire.into()),
            Err(wire_error) => serde_json::from_value::<LegacyWakeVariant>(value)
                .map(Self::from)
                .map_err(|legacy_error| {
                    serde::de::Error::custom(format!(
                        "wake variant matches neither the per-engine shape ({wire_error}) nor \
                         the engine-and-place shape ({legacy_error})"
                    ))
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        ProviderCapability, ProviderDefinitionVariant, DEFAULT_THRESHOLD_PERCENT,
    };
    use super::*;

    fn wake(variant: WakeVariant) -> ProviderDefinitionVariant {
        ProviderDefinitionVariant::Wake { variant }
    }

    fn parse(value: serde_json::Value) -> WakeVariant {
        let variant: ProviderDefinitionVariant =
            serde_json::from_value(serde_json::json!({ "type": "wake", "variant": value }))
                .expect("deserialize");
        let ProviderDefinitionVariant::Wake { variant } = variant else {
            panic!("a `wake` tag deserializes to a wake definition");
        };
        variant
    }

    #[test]
    fn wake_definitions_supply_the_wake_capability_wherever_they_detect() {
        // Where detection runs is a deployment choice, not a different kind of
        // stage: a pipeline naming any of these has a wake word stage.
        let remote = wake(WakeVariant::OpenWakeWord {
            runtime: WakeRuntime::Wyoming {
                url: "tcp://openwakeword:10400".to_owned(),
                threshold_percent: DEFAULT_THRESHOLD_PERCENT,
            },
            phrases: vec!["hey jarvis".to_owned()],
        });
        let in_process = wake(WakeVariant::OpenWakeWord {
            runtime: WakeRuntime::Local {
                models_dir: None,
                threshold_percent: DEFAULT_THRESHOLD_PERCENT,
            },
            phrases: vec!["hey jarvis".to_owned()],
        });
        let on_device = wake(WakeVariant::MicroWakeWord {
            runtime: MicroWakeWordRuntime::Device,
            phrases: vec!["okay nabu".to_owned()],
        });

        assert_eq!(remote.capability(), ProviderCapability::Wake);
        assert_eq!(in_process.capability(), ProviderCapability::Wake);
        assert_eq!(on_device.capability(), ProviderCapability::Wake);
    }

    #[test]
    fn a_wake_definition_that_omits_its_threshold_reads_as_the_documented_default() {
        let variant = parse(serde_json::json!({
            "type": "openwakeword",
            "runtime": { "where": "wyoming", "url": "tcp://openwakeword:10400" },
        }));

        assert_eq!(variant.threshold_percent(), Some(DEFAULT_THRESHOLD_PERCENT));
        assert!(variant.phrases().is_empty(), "no phrases named means whatever was loaded");
    }

    #[test]
    fn a_satellite_has_nothing_left_to_score() {
        // The device already decided, so there is no threshold to tune — and
        // the type is what says so, rather than a field everyone ignores.
        let variant = parse(
            serde_json::json!({ "type": "microwakeword", "runtime": { "where": "device" } }),
        );

        assert_eq!(variant.threshold_percent(), None);
        assert_eq!(variant.wyoming_url(), None);
        assert_eq!(variant.engine(), WakeEngine::MicroWakeWord);
    }

    #[test]
    fn microwakeword_on_a_server_still_scores() {
        // A Wyoming microWakeWord server reports probabilities like any other,
        // so the threshold an operator tunes has to survive the split.
        let variant = parse(serde_json::json!({
            "type": "microwakeword",
            "runtime": { "where": "wyoming", "url": "tcp://microwakeword:10400",
                         "threshold_percent": 70 },
        }));

        assert_eq!(variant.threshold_percent(), Some(70));
        assert_eq!(variant.wyoming_url(), Some("tcp://microwakeword:10400"));
    }

    #[test]
    fn a_local_definition_may_leave_the_model_directory_to_convention() {
        let named = parse(serde_json::json!({
            "type": "nanowakeword",
            "runtime": { "where": "local", "models_dir": "/models" },
        }));
        let conventional = parse(
            serde_json::json!({ "type": "nanowakeword", "runtime": { "where": "local" } }),
        );

        assert_eq!(named.local_models_dir(), Some(Some("/models")));
        assert_eq!(conventional.local_models_dir(), Some(None));
        assert_eq!(named.engine(), WakeEngine::NanoWakeWord);
    }

    #[test]
    fn a_definition_written_before_the_split_still_reads() {
        // The engine and the place used to be independent fields. A stored
        // record saying so describes a provider that still works.
        let variant = parse(serde_json::json!({
            "type": "wyoming",
            "url": "tcp://openwakeword:10400",
            "engine": "openwakeword",
            "phrases": ["hey jarvis"],
            "threshold_percent": 70,
        }));

        assert_eq!(
            variant,
            WakeVariant::OpenWakeWord {
                runtime: WakeRuntime::Wyoming {
                    url: "tcp://openwakeword:10400".to_owned(),
                    threshold_percent: 70,
                },
                phrases: vec!["hey jarvis".to_owned()],
            }
        );
    }

    #[test]
    fn a_satellite_written_before_the_split_still_reads() {
        let variant = parse(serde_json::json!({
            "type": "device",
            "engine": "microwakeword",
            "phrases": ["okay nabu"],
        }));

        assert_eq!(
            variant,
            WakeVariant::MicroWakeWord {
                runtime: MicroWakeWordRuntime::Device,
                phrases: vec!["okay nabu".to_owned()],
            }
        );
    }

    #[test]
    fn an_engine_too_large_for_a_satellite_reads_as_detecting_in_process() {
        // Validation always refused this, so it only exists in a hand-written
        // file — and refusing it here would take startup down over a
        // definition the operator can no longer reach to fix.
        let variant = parse(serde_json::json!({
            "type": "device",
            "engine": "openwakeword",
            "phrases": ["hey jarvis"],
        }));

        assert_eq!(
            variant,
            WakeVariant::OpenWakeWord {
                runtime: WakeRuntime::Local {
                    models_dir: None,
                    threshold_percent: DEFAULT_THRESHOLD_PERCENT,
                },
                phrases: vec!["hey jarvis".to_owned()],
            }
        );
    }

    #[test]
    fn a_definition_is_written_back_in_the_per_engine_shape() {
        // Reading the old shape is a kindness to stored records; writing it
        // back is not. Saving upgrades the record in place.
        let variant = parse(serde_json::json!({
            "type": "wyoming",
            "url": "tcp://openwakeword:10400",
            "engine": "nanowakeword",
        }));

        let written = serde_json::to_value(&variant).expect("serialize");
        assert_eq!(
            written,
            serde_json::json!({
                "type": "nanowakeword",
                "runtime": {
                    "where": "wyoming",
                    "url": "tcp://openwakeword:10400",
                    "threshold_percent": DEFAULT_THRESHOLD_PERCENT,
                },
                "phrases": [],
            })
        );
        assert_eq!(parse(written), variant, "and reads back as what it was");
    }

    #[test]
    fn a_variant_that_matches_neither_shape_says_so_about_both() {
        let error = serde_json::from_value::<WakeVariant>(serde_json::json!({
            "type": "porcupine",
            "url": "tcp://porcupine:10400",
        }))
        .expect_err("porcupine is not an engine Conduit speaks to");

        let message = error.to_string();
        assert!(message.contains("per-engine shape"), "{message}");
        assert!(message.contains("engine-and-place shape"), "{message}");
    }
}
