//! Checking a voice id before it becomes part of a URL path.
//!
//! Synthesis addresses a voice in the *path*:
//! `POST /v1/text-to-speech/{voice_id}/stream`. That makes the voice id the one
//! request component that can move the request to a different endpoint rather
//! than change what the endpoint is asked for — and a voice id is not a
//! constant in this codebase. It arrives from a stored provider definition, a
//! pipeline node's settings, or a synthesis request, any of which an operator
//! (or something acting as one) can write.
//!
//! A value like `../../voices/xyz` or `..%2f..%2fvoices` therefore has to be
//! refused *here*, before it reaches a URL. Percent-encoding the segment would
//! also stop traversal, but it would silently turn a typo into a 404 from a
//! path nobody meant to call; refusing names the field instead.
//!
//! The check is an allowlist rather than a denylist, because a denylist has to
//! anticipate every spelling of "go up a level" — `..`, `%2e%2e`, `..%5c`, a
//! backslash on a server that normalises it, an overlong UTF-8 encoding — and
//! an allowlist has to anticipate nothing. ElevenLabs voice ids are 20-odd
//! ASCII alphanumeric characters (`21m00Tcm4TlvDq8ikWAM`, say), so
//! "alphanumeric, with `-` and `_`, and nothing else" admits every real id and
//! no path at all: `/`, `\`, `.`, `%`, and `?` are all outside it.

use conduit_core::{Error, Result};

/// The longest voice id this crate will accept.
///
/// Real ids are around 20 characters. The bound is generous rather than exact
/// because the vendor is free to lengthen them, and it exists to stop a
/// megabyte of alphanumerics being pasted into a URL rather than to guess the
/// vendor's format.
const MAX_LENGTH: usize = 64;

/// Checks that `voice_id` can be a single URL path segment and nothing else.
///
/// # Errors
///
/// Returns [`Error::Config`] naming the `voice_id` field when the value is
/// empty, over [`MAX_LENGTH`], or contains anything outside
/// `[A-Za-z0-9_-]` — which is every character that could make it a path rather
/// than a name.
pub fn validate(voice_id: &str) -> Result<&str> {
    if voice_id.is_empty() {
        return Err(Error::Config(
            "the `voice_id` field is empty; name the ElevenLabs voice to speak with".to_owned(),
        ));
    }
    if voice_id.len() > MAX_LENGTH {
        return Err(Error::Config(format!(
            "the `voice_id` field is {} characters, over the {MAX_LENGTH} allowed; \
             an ElevenLabs voice id is around 20, e.g. `21m00Tcm4TlvDq8ikWAM`",
            voice_id.len()
        )));
    }
    if let Some(bad) = voice_id.chars().find(|character| !is_allowed(*character)) {
        // The offending character is quoted rather than the whole value: a
        // rejected id is attacker-influenced text, and `{bad:?}` escapes
        // whatever it is. The value itself is not a secret, but it is also not
        // something to paste into a log verbatim.
        return Err(Error::Config(format!(
            "the `voice_id` field contains {bad:?}, which is not allowed: a voice id must be \
             ASCII letters, digits, `-`, or `_`, because it is used as a URL path segment"
        )));
    }
    Ok(voice_id)
}

/// Whether `character` may appear in a voice id.
///
/// Deliberately narrow. `-` and `_` are here because vendors reach for them in
/// generated identifiers, and neither can form a path.
const fn is_allowed(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_id_can_never_escape_its_path_segment() {
        // The guarantee this module exists for. A voice id reaches here from a
        // stored provider definition or a pipeline setting, and it is
        // interpolated into `/v1/text-to-speech/{voice_id}/stream` — so a value
        // that can express "go up a level" can redirect the request to another
        // API path with the caller's credential attached.
        //
        // Every spelling of traversal must be refused, not sanitised: raw,
        // percent-encoded, double-encoded, backslashed, and absolute.
        let attempts = [
            "../../voices",
            "..",
            "../",
            "a/../b",
            "..%2f..%2fvoices",
            "%2e%2e%2fvoices",
            "%252e%252e%252f",
            "..\\..\\voices",
            "/v1/voices",
            "voice/../../user",
            "voice?query=1",
            "voice#fragment",
            "voice%00",
            "voice with space",
            "voice\nInjected-Header: yes",
            "..;/voices",
        ];
        for attempt in attempts {
            let error =
                validate(attempt).expect_err(&format!("`{attempt}` must not reach a URL path"));
            assert!(
                matches!(error, Error::Config(_)),
                "`{attempt}` must be refused as configuration: {error}"
            );
            assert!(
                error.to_string().contains("voice_id"),
                "the error must name the field so an operator can find it: {error}"
            );
        }
    }

    #[test]
    fn real_voice_ids_are_accepted() {
        // Refusing traversal is worthless if it also refuses the ids the vendor
        // actually issues. These are documented examples.
        for id in ["21m00Tcm4TlvDq8ikWAM", "9BWtsMINqrJLrRacOk9x", "DCwhRBWXzGAHq8TQ4Fs18"] {
            assert_eq!(validate(id).expect("a documented voice id"), id);
        }
    }

    #[test]
    fn hyphens_and_underscores_are_allowed_but_dots_and_slashes_are_not() {
        assert!(validate("voice-one_two").is_ok());
        assert!(validate("voice.one").is_err(), "a dot is half of `..`");
        assert!(validate("voice/one").is_err());
    }

    #[test]
    fn an_empty_voice_id_is_refused_rather_than_producing_a_doubled_slash() {
        // `/v1/text-to-speech//stream` is a different route, and an empty
        // configured voice is a mistake worth reporting.
        let error = validate("").expect_err("empty");
        assert!(error.to_string().contains("voice_id"), "{error}");
    }

    #[test]
    fn an_absurdly_long_voice_id_is_refused() {
        let error = validate(&"a".repeat(MAX_LENGTH + 1)).expect_err("too long");
        assert!(error.to_string().contains("voice_id"), "{error}");
        assert!(validate(&"a".repeat(MAX_LENGTH)).is_ok(), "the bound itself is allowed");
    }

    #[test]
    fn non_ascii_that_normalises_to_a_separator_is_refused() {
        // A fullwidth solidus and a unicode fraction slash are not `/` to Rust,
        // but a server or proxy that normalises them would make them one.
        for id in ["voice／one", "voice⁄one", "voiceⅰ"] {
            assert!(validate(id).is_err(), "`{id}` must be refused");
        }
    }
}
