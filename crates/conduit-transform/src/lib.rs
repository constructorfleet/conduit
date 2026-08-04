//! Built-in utterance transforms for Conduit.
//!
//! A model writes for a reader; a synthesizer speaks to a listener. The rules
//! here are the difference between the two, applied to what the model said on
//! its way to being spoken — so that a pipeline stops depending on the model
//! honouring "do not use emoji" and starts depending on something that cannot
//! decline.
//!
//! Every rule is a pure function of one segment, and [`Builtin`] runs them in
//! the order a provider definition lists them. Order matters: flattening
//! markdown before stripping emoji means an emoji inside a link's text is seen
//! as text, and the other way around means it is seen as part of an address.

pub mod emoji;
pub mod markdown;
pub mod whitespace;

use conduit_core::Result;
use conduit_provider::storage::Rule;
use conduit_provider::transform::UtteranceTransform;
use conduit_provider::{Capability, Descriptor, Provider};

/// An utterance transform built from the rules that ship with Conduit.
///
/// Holds no connection and no state, so it is cheap to register and safe to
/// share across every turn that names it.
#[derive(Debug, Clone)]
pub struct Builtin {
    descriptor: Descriptor,
    rules: Vec<Rule>,
}

impl Builtin {
    /// Creates a transform that applies `rules` in order.
    ///
    /// An empty rule list is allowed and does nothing. It is what a definition
    /// looks like while an operator is still deciding, and refusing it would
    /// make a half-filled form unsaveable.
    #[must_use]
    pub fn new(name: impl Into<String>, rules: Vec<Rule>) -> Self {
        Self { descriptor: Descriptor::new(name, Capability::Transform), rules }
    }

    /// Sets the human-readable name operator screens show.
    ///
    /// Separate from the identity this provider was built with: the identity
    /// is what a pipeline selects and what appears in metric labels, and this
    /// is only what a person reads.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.with_label(label);
        self
    }

    /// The rules this transform applies, in order.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

#[async_trait::async_trait]
impl Provider for Builtin {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
}

#[async_trait::async_trait]
impl UtteranceTransform for Builtin {
    async fn transform(&self, segment: &str) -> Result<String> {
        Ok(self.rules.iter().fold(segment.to_owned(), |text, rule| apply(*rule, &text)))
    }
}

/// Applies one rule to one segment.
fn apply(rule: Rule, text: &str) -> String {
    match rule {
        Rule::StripEmoji => emoji::strip(text),
        Rule::MarkdownToSpeech => markdown::flatten(text),
        Rule::CollapseWhitespace => whitespace::collapse(text),
        // `Rule` is `non_exhaustive`, so a rule added in a later release that
        // this build does not implement reaches here. Speaking the segment
        // unchanged is the honest outcome: the operator asked for something
        // this binary cannot do, and dropping the words would hide it.
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn spoken(rules: Vec<Rule>, segment: &str) -> String {
        Builtin::new("clean", rules)
            .transform(segment)
            .await
            .expect("built-in rules cannot fail")
    }

    #[tokio::test]
    async fn a_transform_with_no_rules_leaves_the_segment_alone() {
        assert_eq!(spoken(Vec::new(), "**Hello** 👋").await, "**Hello** 👋");
    }

    #[tokio::test]
    async fn rules_are_applied_in_the_order_they_are_listed() {
        let segment = "Check [the docs 📚](https://example.com) now";
        let rules = vec![Rule::MarkdownToSpeech, Rule::StripEmoji];
        assert_eq!(spoken(rules, segment).await, "Check the docs now");
    }

    #[tokio::test]
    async fn a_segment_left_with_nothing_to_say_comes_back_empty() {
        assert_eq!(spoken(vec![Rule::StripEmoji], "🎉🎉").await, "");
    }

    #[tokio::test]
    async fn the_common_pairing_reads_as_prose() {
        let segment =
            "## Forecast ☀️\n\n- **8am**: `12°C`, see [details](https://example.com/x)";
        let rules = vec![Rule::MarkdownToSpeech, Rule::StripEmoji];
        assert_eq!(spoken(rules, segment).await, "Forecast 8am: 12°C, see details");
    }

    #[tokio::test]
    async fn a_transform_reports_the_name_it_was_registered_under() {
        assert_eq!(Provider::name(&Builtin::new("clean", Vec::new())), "clean");
    }
}
