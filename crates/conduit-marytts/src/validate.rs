//! What may be put in a request to the synthesis endpoint.
//!
//! `VOICE` and `LOCALE` are chosen by whoever configured the provider or sent
//! the turn, and both reach MaryTTS as request parameters. Neither is ever a
//! free-form string as far as the server is concerned: a voice is the name of
//! something installed on it and a locale is a Java locale tag. So both are
//! checked against a strict allowlist of what those things can look like,
//! rather than escaped — an allowlist states what is permitted, and an escape
//! only states what one author remembered to escape.

use conduit_core::{Error, Result};

/// The longest a voice name may be.
///
/// Installed MaryTTS voices are names like `cmu-slt-hsmm`. The bound is here so
/// that a caller cannot push an unbounded string into a request at all.
const MAX_VOICE: usize = 64;

/// The longest a locale tag may be, generously: `language_REGION_variant`.
const MAX_LOCALE: usize = 32;

/// Checks a voice name, returning it unchanged.
///
/// Permitted characters are ASCII letters and digits, `-`, `_`, and `.`, which
/// covers every voice MaryTTS ships as well as the `male` and `female`
/// selectors the endpoint also accepts. Everything else — a separator, a
/// space, a newline, a percent escape — is refused.
///
/// # Errors
///
/// Returns [`Error::Config`] naming `voice` as the offending field.
pub fn voice(provider: &str, value: &str) -> Result<String> {
    if value.is_empty() {
        return Err(rejected(provider, "voice", value, "it is empty"));
    }
    if value.len() > MAX_VOICE {
        return Err(rejected(
            provider,
            "voice",
            value,
            &format!("it is longer than {MAX_VOICE} characters"),
        ));
    }
    if let Some(bad) = value.chars().find(|character| !is_voice_character(*character)) {
        return Err(rejected(
            provider,
            "voice",
            value,
            &format!("{bad:?} is not a letter, digit, `-`, `_`, or `.`"),
        ));
    }
    Ok(value.to_owned())
}

/// Checks a locale tag, returning it in the underscore form MaryTTS expects.
///
/// A tag is a two- or three-letter language, optionally followed by a region
/// and a variant. Conduit speaks BCP-47 (`en-US`) and Java speaks
/// `Locale.toString()` (`en_US`), so a hyphenated tag is accepted and
/// normalized rather than refused: the two name the same locale, and making an
/// operator remember which side of the wire they are on is a trap, not a
/// safety property.
///
/// # Errors
///
/// Returns [`Error::Config`] naming `locale` as the offending field.
pub fn locale(provider: &str, value: &str) -> Result<String> {
    if value.is_empty() {
        return Err(rejected(provider, "locale", value, "it is empty"));
    }
    if value.len() > MAX_LOCALE {
        return Err(rejected(
            provider,
            "locale",
            value,
            &format!("it is longer than {MAX_LOCALE} characters"),
        ));
    }

    // Normalized before checking, so `en-US` and `en_US` are held to the same
    // rule rather than one of them slipping through a different branch.
    let normalized = value.replace('-', "_");
    let mut segments = normalized.split('_');

    let language = segments.next().unwrap_or_default();
    if !(2..=3).contains(&language.len()) || !language.chars().all(|c| c.is_ascii_alphabetic())
    {
        return Err(rejected(
            provider,
            "locale",
            value,
            "it does not start with a two- or three-letter language",
        ));
    }

    for segment in segments {
        // An empty segment is `en__US` or a trailing separator, which is not a
        // locale and would reach the server as one.
        if segment.is_empty() || !segment.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(rejected(
                provider,
                "locale",
                value,
                "its region and variant must be letters and digits",
            ));
        }
    }

    Ok(normalized)
}

/// Checks that there is something to say.
///
/// An empty utterance is a round trip that can only come back empty, and the
/// caller learns more from being told here than from a server that synthesizes
/// silence.
///
/// # Errors
///
/// Returns [`Error::Config`] naming `text` as the offending field.
pub fn text(provider: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(rejected(provider, "text", value, "there is nothing to speak"));
    }
    Ok(())
}

/// Whether `character` may appear in a voice name.
fn is_voice_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

/// A rejection naming the field, so an operator knows what to fix.
///
/// The offending value is quoted with [`Debug`], which escapes a newline
/// rather than letting it break the log line it lands in.
fn rejected(provider: &str, field: &str, value: &str, why: &str) -> Error {
    Error::Config(format!("provider `{provider}` rejected `{field}` {value:?}: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_voices_marytts_actually_ships_are_accepted() {
        for name in ["cmu-slt-hsmm", "dfki-prudence-hsmm", "bits1-hsmm", "male", "female"] {
            assert_eq!(voice("marytts", name).expect("accepted"), name);
        }
    }

    #[test]
    fn a_voice_carrying_a_parameter_separator_is_refused_rather_than_sent() {
        // The guarantee: a voice name cannot smuggle a second request
        // parameter. `&OUTPUT_TYPE=...` in a voice would otherwise reach the
        // endpoint as its own parameter and redirect what the server does.
        let error = voice("marytts", "cmu-slt-hsmm&OUTPUT_TYPE=TEXT").expect_err("refused");
        assert!(matches!(error, Error::Config(_)));
        assert!(error.to_string().contains("`voice`"), "names the field: {error}");
    }

    #[test]
    fn a_voice_attempting_any_other_injection_is_refused_rather_than_sent() {
        // Each of these is a way of ending the value early or of hiding what
        // follows it, and none of them is a voice name.
        for attempt in [
            "voice=x&AUDIO=MP3_FILE",
            "voice?INPUT_TEXT=other",
            "voice#fragment",
            "voice with space",
            "voice%26AUDIO",
            "voice\nINPUT_TEXT=other",
            "voice\r\nHost: elsewhere",
            "../../etc/passwd",
            "voice/../..",
            "voice;drop",
            "\u{202e}voice",
        ] {
            let error = voice("marytts", attempt).expect_err("refused");
            assert!(error.to_string().contains("`voice`"), "{attempt:?}: {error}");
        }
    }

    #[test]
    fn a_locale_attempting_an_injection_is_refused_rather_than_sent() {
        for attempt in [
            "en_US&VOICE=other",
            "en_US?x=1",
            "en US",
            "en_US\nVOICE=other",
            "../en_US",
            "e",
            "english_US",
            "en__US",
            "en_",
            "_US",
            "",
        ] {
            let error = locale("marytts", attempt).expect_err("refused");
            assert!(error.to_string().contains("`locale`"), "{attempt:?}: {error}");
        }
    }

    #[test]
    fn an_unbounded_value_is_refused_before_it_reaches_a_request() {
        assert!(voice("marytts", &"a".repeat(MAX_VOICE + 1)).is_err());
        assert!(locale("marytts", &"a".repeat(MAX_LOCALE + 1)).is_err());
    }

    #[test]
    fn a_bcp_47_tag_is_normalized_to_the_form_the_server_expects() {
        // Conduit says `en-US` and Java says `en_US`; both name one locale.
        assert_eq!(locale("marytts", "en-US").expect("accepted"), "en_US");
        assert_eq!(locale("marytts", "en_US").expect("accepted"), "en_US");
        assert_eq!(locale("marytts", "de").expect("accepted"), "de");
        assert_eq!(locale("marytts", "en_GB").expect("accepted"), "en_GB");
    }

    #[test]
    fn a_three_letter_language_and_a_variant_are_still_locales() {
        assert_eq!(locale("marytts", "fil_PH").expect("accepted"), "fil_PH");
        assert_eq!(locale("marytts", "sr_RS_latin").expect("accepted"), "sr_RS_latin");
    }

    #[test]
    fn an_utterance_with_no_words_is_refused_before_a_round_trip() {
        assert!(text("marytts", "").is_err());
        assert!(text("marytts", "   \n\t ").is_err());
        assert!(text("marytts", "hello").is_ok());
    }

    #[test]
    fn a_rejected_value_never_breaks_the_line_it_is_reported_on() {
        // The value is quoted with `Debug`, so a newline in it cannot forge a
        // second log line.
        let error = voice("marytts", "x\nINPUT_TEXT=other").expect_err("refused");
        assert!(!error.to_string().contains('\n'), "{error:?}");
    }
}
