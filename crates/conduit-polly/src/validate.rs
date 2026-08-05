//! Checking a definition's fields before a request carries them to AWS.
//!
//! Here rather than in `conduit-api` so the rule and the code that depends on it
//! live together: the engine names are the SDK's, and this crate is the one that
//! knows them.
//!
//! Not compiled out with the `polly` feature. A build without the SDK still stores
//! and serves definitions, and an operator saving one on such a host should get the
//! same field-level refusal they would get anywhere else.

use conduit_core::{Error, Result};

/// The engines Polly synthesizes with.
///
/// Read off `aws_sdk_polly::types::Engine` rather than a docs page, and listed here
/// so the check exists in a build compiled without the SDK.
pub const ENGINES: [&str; 4] = ["generative", "long-form", "neural", "standard"];

/// Checks that `engine` is one Polly offers.
///
/// A closed set, unlike a voice: there are four, they are the same in every
/// region, and a typo here fails every turn rather than one.
///
/// # Errors
///
/// Returns [`Error::Config`] naming the engines if `engine` is not one of them.
pub fn engine(provider: &str, engine: &str) -> Result<()> {
    if ENGINES.contains(&engine) {
        return Ok(());
    }
    Err(Error::Config(format!(
        "provider `{provider}` names engine `{engine}`, which Polly does not have; it offers {}",
        ENGINES.join(", ")
    )))
}

/// Checks that `voice` is shaped like a Polly voice id.
///
/// A shape check rather than a list, on the same terms as the region check in
/// `conduit-api`: the 106 voices are AWS's to add, and a build that refused a voice
/// added after it was compiled would be worse than one that let the API say so.
/// What this catches is the real mistake — a voice name typed as `en-US-Neural2-F`,
/// which is Google's spelling, or left with the surrounding quotes.
///
/// # Errors
///
/// Returns [`Error::Config`] if `voice` is empty, over 64 characters, or carries
/// anything but ASCII letters and digits.
pub fn voice(provider: &str, voice: &str) -> Result<()> {
    if !voice.is_empty()
        && voice.len() <= 64
        && voice.chars().all(|character| character.is_ascii_alphanumeric())
    {
        return Ok(());
    }
    Err(Error::Config(format!(
        "provider `{provider}` names voice `{voice}`; a Polly voice id is a single \
         capitalized name such as `Joanna`, not a language-tagged model name"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_engines_polly_has_are_accepted() {
        for name in ENGINES {
            engine("house", name).expect("an engine Polly offers");
        }
    }

    #[test]
    fn an_engine_polly_does_not_have_is_refused_by_listing_the_ones_it_does() {
        // `turbo` is a plausible guess from another vendor's vocabulary, and it
        // would otherwise fail every turn with a vendor error.
        let error = engine("house", "turbo").expect_err("not an engine").to_string();

        assert!(error.contains("turbo"), "what was asked for: {error}");
        assert!(error.contains("neural"), "and what is available: {error}");
    }

    #[test]
    fn a_voice_id_is_accepted_without_being_on_a_list() {
        // A voice added after this build shipped must still work.
        voice("house", "Joanna").expect("shaped like a voice id");
        voice("house", "Aria").expect("shaped like a voice id");
        voice("house", "SomeVoiceAwsAddsLater").expect("not checked against a list");
    }

    #[test]
    fn another_vendors_voice_spelling_is_refused() {
        for wrong in ["en-US-Neural2-F", "\"Joanna\"", "", "aura-2-thalia-en"] {
            voice("house", wrong).expect_err("not a Polly voice id");
        }
    }
}
