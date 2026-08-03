//! Wake word detection provider variants.

use serde::{Deserialize, Serialize};

use super::{default_threshold_percent, WakeEngine};

/// Wake word detection provider variants.
///
/// Where detection runs is a deployment choice, not a different kind of stage:
/// a pipeline naming either definition has a wake word stage. The Wyoming
/// variant points at a server; the device variant names a satellite that wakes
/// itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WakeVariant {
    /// Wake word detection on a Wyoming server.
    ///
    /// All three engines are packaged as Wyoming services, so one variant
    /// serves them and [`WakeEngine`] says which is listening.
    Wyoming {
        /// Wyoming endpoint URL.
        url: String,
        /// Which detector is behind the endpoint.
        engine: WakeEngine,
        /// Phrases to listen for. Empty asks the server for whatever it loaded.
        #[serde(default)]
        phrases: Vec<String>,
        /// Minimum confidence to accept, as a percentage.
        #[serde(default = "default_threshold_percent")]
        threshold_percent: u8,
    },
    /// Wake word detection performed on the satellite itself.
    ///
    /// There is no endpoint because there is no server: the device runs the
    /// detector and only streams audio once it has activated. The definition
    /// exists so that a pipeline can *say* it wakes on-device — which is what
    /// makes the stage visible in the editor, in validation, and in the event
    /// stream — rather than the stage silently being absent.
    Device {
        /// Which detector the satellite runs.
        engine: WakeEngine,
        /// Phrases the satellite is flashed with, for operator screens. The
        /// server never scores them.
        #[serde(default)]
        phrases: Vec<String>,
    },
}

impl WakeVariant {
    /// Returns a copy with inline secrets redacted.
    pub(super) fn redacted(&self) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        ProviderCapability, ProviderDefinitionVariant, DEFAULT_THRESHOLD_PERCENT,
    };
    use super::*;

    #[test]
    fn wake_definitions_supply_the_wake_capability_wherever_they_detect() {
        // Where detection runs is a deployment choice, not a different kind of
        // stage: a pipeline naming either definition has a wake word stage.
        let remote = ProviderDefinitionVariant::Wake {
            variant: WakeVariant::Wyoming {
                url: "tcp://openwakeword:10400".to_owned(),
                engine: WakeEngine::OpenWakeWord,
                phrases: vec!["hey jarvis".to_owned()],
                threshold_percent: DEFAULT_THRESHOLD_PERCENT,
            },
        };
        let on_device = ProviderDefinitionVariant::Wake {
            variant: WakeVariant::Device {
                engine: WakeEngine::MicroWakeWord,
                phrases: vec!["okay nabu".to_owned()],
            },
        };

        assert_eq!(remote.capability(), ProviderCapability::Wake);
        assert_eq!(on_device.capability(), ProviderCapability::Wake);
    }

    #[test]
    fn a_wake_definition_that_omits_its_threshold_reads_as_the_documented_default() {
        let variant: ProviderDefinitionVariant = serde_json::from_value(serde_json::json!({
            "type": "wake",
            "variant": {
                "type": "wyoming",
                "url": "tcp://openwakeword:10400",
                "engine": "openwakeword",
            },
        }))
        .expect("deserialize");

        let ProviderDefinitionVariant::Wake {
            variant: WakeVariant::Wyoming { threshold_percent, phrases, .. },
        } = variant
        else {
            panic!("a `wake`/`wyoming` tag deserializes to a Wyoming wake definition");
        };
        assert_eq!(threshold_percent, DEFAULT_THRESHOLD_PERCENT);
        assert!(phrases.is_empty(), "no phrases named means whatever the server loaded");
    }

    #[test]
    fn only_microwakeword_is_small_enough_for_a_satellite() {
        assert!(WakeEngine::MicroWakeWord.runs_on_device());
        assert!(!WakeEngine::OpenWakeWord.runs_on_device());
        assert!(!WakeEngine::NanoWakeWord.runs_on_device());
    }
}
