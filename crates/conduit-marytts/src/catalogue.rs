//! Reading the plain-text catalogues the server publishes.
//!
//! MaryTTS predates the assumption that an API answers in JSON. `GET /voices`
//! and `GET /locales` return `text/plain`, one item per line, space-separated —
//! so the parsing lives here, with the tolerance a hand-written format needs.
//!
//! From `MaryRuntimeUtils.getVoices()`, a voice line is:
//!
//! ```text
//! cmu-slt-hsmm en_US female hmm
//! dfki-pavoque-neutral de male unitselection general
//! ```
//!
//! Name, locale, gender, and type, with a fifth domain field on unit-selection
//! voices. Only the first two matter here; the rest is read past rather than
//! required, because a field this crate does not use is not a reason to refuse
//! a voice an operator has installed.

use conduit_provider::tts::Voice;

/// Reads a `GET /voices` body into a catalogue.
///
/// Lines that are blank or that carry no locale are skipped rather than
/// failing the parse: the catalogue is descriptive, and one unreadable line
/// from a server build this crate has not seen should not empty the list an
/// operator picks a voice from.
#[must_use]
pub fn voices(body: &str) -> Vec<Voice> {
    body.lines().filter_map(voice).collect()
}

/// Reads one line of a voice listing.
fn voice(line: &str) -> Option<Voice> {
    let mut fields = line.split_whitespace();
    let id = fields.next()?;
    let locale = fields.next().unwrap_or_default();

    // The language is reported as BCP-47 because that is what `Voice::language`
    // is documented to hold and what every other provider puts there; the
    // server speaks Java's underscore form.
    Some(Voice { id: id.to_owned(), name: id.to_owned(), language: locale.replace('_', "-") })
}

/// Reads a `GET /locales` body into BCP-47 tags.
///
/// The server lists one Java locale per line. Empty lines are skipped.
#[must_use]
pub fn locales(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.replace('_', "-"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `/voices` body in the shape `MaryRuntimeUtils.getVoices()` writes it:
    /// an HMM voice, a unit-selection voice with its trailing domain field, and
    /// a voice of neither kind.
    const BODY: &str = "cmu-slt-hsmm en_US female hmm\n\
         dfki-pavoque-neutral de male unitselection general\n\
         upmc-pierre-hsmm fr male other\n";

    #[test]
    fn every_voice_the_server_lists_is_offered_with_its_language() {
        let voices = voices(BODY);
        assert_eq!(voices.len(), 3);

        assert_eq!(voices[0].id, "cmu-slt-hsmm");
        assert_eq!(voices[0].name, "cmu-slt-hsmm");
        assert_eq!(voices[0].language, "en-US", "reported as BCP-47, not Java's en_US");

        // The fifth `general` field belongs to unit-selection voices and is
        // nothing this crate needs; the voice is still usable.
        assert_eq!(voices[1].id, "dfki-pavoque-neutral");
        assert_eq!(voices[1].language, "de");

        assert_eq!(voices[2].id, "upmc-pierre-hsmm");
        assert_eq!(voices[2].language, "fr");
    }

    #[test]
    fn a_catalogue_is_not_emptied_by_one_line_it_cannot_read() {
        // A server build this crate has not seen writes a line it does not
        // expect. Refusing the whole body would leave an operator with no
        // voices at all, which is worse than skipping one.
        let voices = voices("cmu-slt-hsmm en_US female hmm\n\n   \nbroken\n");
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].id, "cmu-slt-hsmm");
        // A name with no locale is still a name that can be sent as `VOICE`.
        assert_eq!(voices[1].id, "broken");
        assert!(voices[1].language.is_empty());
    }

    #[test]
    fn an_empty_body_is_a_server_with_no_voices_rather_than_an_error() {
        assert!(voices("").is_empty());
        assert!(voices("\n\n").is_empty());
    }

    #[test]
    fn windows_line_endings_do_not_become_part_of_a_voice_name() {
        // A name with a trailing `\r` would be refused by the validator later,
        // turning a usable server into an unusable one.
        let voices = voices("cmu-slt-hsmm en_US female hmm\r\n");
        assert_eq!(voices[0].id, "cmu-slt-hsmm");
        assert_eq!(voices[0].language, "en-US");
    }

    #[test]
    fn locales_are_read_as_bcp_47_tags() {
        assert_eq!(locales("en_US\nde\nfr\n"), ["en-US", "de", "fr"]);
        assert_eq!(locales("en_US\r\n"), ["en-US"]);
        assert!(locales("").is_empty());
    }
}
