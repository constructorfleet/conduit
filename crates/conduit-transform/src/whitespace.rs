//! Normalising the spacing of text that is about to be spoken.

/// Collapses every run of whitespace in `text` to a single space and trims the
/// ends.
///
/// Line breaks go with it. A synthesizer is given one segment at a time and
/// speaks it as one phrase, so the difference between a newline and a space is
/// not something a listener can hear — but it is something a provider may
/// choke on, and it is what a stripped emoji or a removed list marker leaves
/// behind.
#[must_use]
pub fn collapse(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut pending_space = false;

    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !collapsed.is_empty();
            continue;
        }
        if pending_space {
            collapsed.push(' ');
            pending_space = false;
        }
        collapsed.push(character);
    }

    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_of_spaces_becomes_one() {
        assert_eq!(collapse("too    many   spaces"), "too many spaces");
    }

    #[test]
    fn the_ends_are_trimmed() {
        assert_eq!(collapse("  padded  "), "padded");
    }

    #[test]
    fn a_line_break_reads_as_a_space() {
        assert_eq!(collapse("first\nsecond"), "first second");
    }

    #[test]
    fn text_that_is_only_whitespace_becomes_empty() {
        assert_eq!(collapse(" \n\t "), "");
    }

    #[test]
    fn already_tidy_text_is_unchanged() {
        assert_eq!(collapse("nothing to do here"), "nothing to do here");
    }
}
