//! Checking an Aura model id before it becomes a query parameter.
//!
//! Unlike ElevenLabs' voice id, this one is not a URL *path* segment, and
//! `reqwest`'s `query` percent-encodes it — so there is no traversal to prevent
//! and this is not a security boundary. What it is instead: the difference
//! between naming the field an operator got wrong and relaying a vendor 400.
//!
//! The check is deliberately loose. Aura ids are `[family]-[voice]-[language]`
//! today (`aura-2-thalia-en`), but a pattern encoding that shape would refuse
//! every id Deepgram releases in a form this crate did not anticipate — and a
//! provider that refuses a working voice is worse than one that forwards an
//! unknown name and lets the vendor say it does not know it. So this admits
//! anything that could plausibly be an id and rejects only what certainly is
//! not: empty, absurdly long, or carrying characters that mean something to a
//! URL.

use conduit_core::{Error, Result};

/// The longest model id this crate will accept.
///
/// Real ids are around 16 characters. The bound exists to stop a megabyte of
/// text reaching a query string, not to guess the vendor's format.
const MAX_LENGTH: usize = 64;

/// Checks that `model` could be an Aura model id.
///
/// # Errors
///
/// Returns [`Error::Config`] naming the `model` field when the value is empty,
/// over [`MAX_LENGTH`], or contains anything outside `[A-Za-z0-9._-]`.
pub fn validate(model: &str) -> Result<&str> {
    if model.is_empty() {
        return Err(Error::Config(
            "the `model` field is empty; name the Aura voice to speak with, \
             e.g. `aura-2-thalia-en`"
                .to_owned(),
        ));
    }
    if model.chars().count() > MAX_LENGTH {
        return Err(Error::Config(format!(
            "the `model` field is {} characters, over the {MAX_LENGTH} allowed; \
             an Aura model id is around 16, e.g. `aura-2-thalia-en`",
            model.chars().count()
        )));
    }
    if let Some(bad) = model.chars().find(|character| !is_allowed(*character)) {
        // The offending character is quoted rather than the whole value, so
        // whatever it is gets escaped rather than pasted into a log verbatim.
        return Err(Error::Config(format!(
            "the `model` field contains {bad:?}, which is not part of an Aura \
             model id; ids look like `aura-2-thalia-en`"
        )));
    }
    Ok(model)
}

/// Whether `character` may appear in a model id.
const fn is_allowed(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ids_deepgram_publishes_are_accepted() {
        for model in
            ["aura-asteria-en", "aura-2-thalia-en", "aura-2-celeste-es", "aura-2-fujin-ja"]
        {
            assert!(validate(model).is_ok(), "{model} is a real Aura id");
        }
    }

    #[test]
    fn a_shape_this_crate_did_not_anticipate_is_still_forwarded() {
        // The point of the loose check: a provider that refuses a voice the
        // vendor has released is worse than one that forwards an unknown name.
        assert!(validate("aura-3").is_ok());
        assert!(validate("some_future.model-99").is_ok());
    }

    #[test]
    fn an_empty_model_names_the_field_and_gives_an_example() {
        let error = validate("").expect_err("empty is not an id").to_string();

        assert!(error.contains("model"));
        assert!(error.contains("aura-2-thalia-en"), "an example beats a rule: {error}");
    }

    #[test]
    fn something_that_is_not_an_id_at_all_is_refused() {
        for bad in ["aura en", "aura/../models", "aura?model=other", "aura&x=1"] {
            assert!(validate(bad).is_err(), "{bad} is not a model id");
        }
    }

    #[test]
    fn a_very_long_value_is_refused_by_characters_rather_than_bytes() {
        // Counting bytes would refuse a shorter multi-byte value than intended.
        // Neither is a real id; the point is that the bound means what it says.
        assert!(validate(&"a".repeat(64)).is_ok());
        assert!(validate(&"a".repeat(65)).is_err());
    }
}
