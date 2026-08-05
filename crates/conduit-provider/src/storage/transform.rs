//! Utterance transform provider variants.

use serde::{Deserialize, Serialize};

/// Utterance transform provider variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransformVariant {
    /// Rules that ship with Conduit, applied in the order they are listed.
    Builtin {
        /// What to rewrite, in order.
        rules: Vec<Rule>,
    },
    /// A rewrite an operator wrote themselves, run on a sandboxed interpreter.
    ///
    /// The other half of [`Builtin`](Self::Builtin)'s trade: a builtin rule is a
    /// Rust function somebody had to write and release, and this one takes
    /// effect on the next utterance.
    Script {
        /// Which interpreter runs [`source`](Self::Script::source).
        ///
        /// Named in the definition rather than assumed, so a second engine could
        /// be added without every stored script silently changing language. One
        /// engine exists today and the field is still not optional.
        engine: ScriptEngine,
        /// The script, as the operator wrote it.
        source: String,
        /// How long one evaluation may run, in milliseconds.
        ///
        /// A deadline rather than a suggestion: a script that does not finish
        /// must fail its segment, because a transform sits inside the turn loop
        /// and a hang there ends every turn on that pipeline rather than one.
        #[serde(default = "default_script_timeout_ms")]
        timeout_ms: u64,
    },
}

impl TransformVariant {
    /// Returns a copy with inline secrets redacted.
    ///
    /// Neither variant holds a credential — a builtin rule has nothing to hold,
    /// and a script's source is the definition rather than a secret in it — so
    /// this is a clone. The method exists so this variant answers the same
    /// question every other one does.
    pub(super) fn redacted(&self) -> Self {
        self.clone()
    }
}

/// The interpreter a scripted transform runs on.
///
/// Written as the one word the project is named by, like [`WakeEngine`], because
/// an operator reading a stored definition should see what they wrote the script
/// in.
///
/// [`WakeEngine`]: super::WakeEngine
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptEngine {
    /// Rhai: an embedded scripting language for Rust, with the sandbox limits
    /// `conduit-script` applies to it.
    #[serde(rename = "rhai")]
    Rhai,
}

impl ScriptEngine {
    /// The word this engine is written as in a definition.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rhai => "rhai",
        }
    }
}

impl std::fmt::Display for ScriptEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// How long a scripted transform gets when the definition does not say.
///
/// 50ms is generous for string work on a jitted expression and short enough that
/// a runaway script costs one segment rather than a noticeable pause. The bound
/// itself is enforced by `conduit-script`, which refuses anything outside its
/// supported range when the provider is built.
const fn default_script_timeout_ms() -> u64 {
    50
}

/// One rewriting rule that ships with Conduit.
///
/// Named rather than configurable because each is a statement about how speech
/// differs from writing, and those do not vary by deployment: an emoji has no
/// spoken form anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Rule {
    /// Removes pictographs, respacing what is left.
    ///
    /// The answer to a model that keeps ending sentences with a sparkle
    /// despite being asked not to.
    StripEmoji,
    /// Rewrites markdown as the words it wraps: headings, emphasis, lists,
    /// tables, links and code spans become their text.
    MarkdownToSpeech,
    /// Collapses runs of whitespace, including line breaks, to single spaces.
    CollapseWhitespace,
}

impl Rule {
    /// The word this rule is written as in a provider definition.
    ///
    /// The same spelling serde uses, so an error message and the definition an
    /// operator is looking at name the rule the same way.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::StripEmoji => "strip_emoji",
            Self::MarkdownToSpeech => "markdown_to_speech",
            Self::CollapseWhitespace => "collapse_whitespace",
        }
    }
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_builtin_variant_round_trips_through_json() {
        let variant =
            TransformVariant::Builtin { rules: vec![Rule::MarkdownToSpeech, Rule::StripEmoji] };
        let encoded = serde_json::to_string(&variant).expect("serializes");
        assert_eq!(
            encoded,
            r#"{"type":"builtin","rules":["markdown_to_speech","strip_emoji"]}"#
        );
        let decoded: TransformVariant = serde_json::from_str(&encoded).expect("deserializes");
        assert_eq!(decoded, variant);
    }

    #[test]
    fn a_script_variant_round_trips_through_json() {
        let variant = TransformVariant::Script {
            engine: ScriptEngine::Rhai,
            source: "segment.to_upper()".to_owned(),
            timeout_ms: 50,
        };
        let encoded = serde_json::to_string(&variant).expect("serializes");
        assert_eq!(
            encoded,
            r#"{"type":"script","engine":"rhai","source":"segment.to_upper()","timeout_ms":50}"#
        );
        let decoded: TransformVariant = serde_json::from_str(&encoded).expect("deserializes");
        assert_eq!(decoded, variant);
    }

    #[test]
    fn a_script_definition_that_names_no_deadline_gets_one_anyway() {
        // A missing deadline must not mean an unbounded one: a script that never
        // finishes would end every turn on the pipeline rather than one segment.
        let decoded: TransformVariant =
            serde_json::from_str(r#"{"type":"script","engine":"rhai","source":"segment"}"#)
                .expect("deserializes");
        assert_eq!(
            decoded,
            TransformVariant::Script {
                engine: ScriptEngine::Rhai,
                source: "segment".to_owned(),
                timeout_ms: default_script_timeout_ms(),
            }
        );
    }

    #[test]
    fn a_script_engine_is_spelled_the_way_an_operator_writes_it() {
        // A rename orphans every saved definition naming the old spelling, so
        // this is the test that has to fail before that can happen.
        let encoded = serde_json::to_string(&ScriptEngine::Rhai).expect("serializes");
        assert_eq!(encoded, r#""rhai""#);
        assert_eq!(ScriptEngine::Rhai.name(), "rhai");
        assert_eq!(ScriptEngine::Rhai.to_string(), "rhai");
    }

    #[test]
    fn a_script_has_nothing_to_redact_and_survives_redaction_whole() {
        // The source is the definition rather than a secret in it, so redaction
        // must not blank the thing the operator wrote.
        let variant = TransformVariant::Script {
            engine: ScriptEngine::Rhai,
            source: "segment.to_upper()".to_owned(),
            timeout_ms: 25,
        };
        assert_eq!(variant.redacted(), variant);
    }

    #[test]
    fn every_rule_is_spelled_the_way_serde_writes_it() {
        for rule in [Rule::StripEmoji, Rule::MarkdownToSpeech, Rule::CollapseWhitespace] {
            let encoded = serde_json::to_string(&rule).expect("serializes");
            assert_eq!(encoded, format!("\"{}\"", rule.name()));
        }
    }
}
