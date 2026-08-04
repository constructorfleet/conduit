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
}

impl TransformVariant {
    /// Returns a copy with inline secrets redacted.
    ///
    /// Built-in rules hold no credentials; the method exists so this variant
    /// answers the same question every other one does.
    pub(super) fn redacted(&self) -> Self {
        self.clone()
    }
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
    fn every_rule_is_spelled_the_way_serde_writes_it() {
        for rule in [Rule::StripEmoji, Rule::MarkdownToSpeech, Rule::CollapseWhitespace] {
            let encoded = serde_json::to_string(&rule).expect("serializes");
            assert_eq!(encoded, format!("\"{}\"", rule.name()));
        }
    }
}
