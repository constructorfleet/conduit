//! Removing pictographs from text that is about to be spoken.

use crate::whitespace::collapse;

/// Removes every pictographic character from `text`, tidying the gaps.
///
/// Removing a character out of the middle of a sentence leaves two spaces
/// where there was one, so the result is respaced: the sentence should read as
/// though the emoji was never written rather than as though something was cut
/// out of it.
#[must_use]
pub fn strip(text: &str) -> String {
    let kept: String = text.chars().filter(|character| !is_pictographic(*character)).collect();
    collapse(&kept)
}

/// Whether `character` is a pictograph rather than a word.
///
/// Deliberately narrower than "everything Unicode calls a symbol". Currency,
/// arrows, and mathematical operators are read aloud by a synthesizer and are
/// meant to be; the blocks below are the ones whose contents have no spoken
/// form at all.
fn is_pictographic(character: char) -> bool {
    matches!(
        u32::from(character),
        // Zero-width joiner and the combining keycap, which are what bind a
        // sequence like `👨‍👩‍👧` or `1️⃣` together. Left behind they would join
        // the letters that survived.
        0x0000_200D
            | 0x0000_20E3
            // Variation selectors, including the one that asks for emoji
            // presentation of an otherwise textual character.
            | 0x0000_FE00..=0x0000_FE0F
            // Miscellaneous symbols and dingbats: ☀ through ➿.
            | 0x0000_2600..=0x0000_27BF
            // Miscellaneous symbols and arrows: ⬛, ⭐.
            | 0x0000_2B00..=0x0000_2BFF
            // Everything from the mahjong tiles through the extended
            // pictographs, which is the bulk of what a model emits: emoticons,
            // transport, flags, supplemental and extended symbols.
            | 0x0001_F000..=0x0001_FAFF
            // Tag characters, which spell out subdivision flags.
            | 0x000E_0020..=0x000E_007F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emoji_between_words_leaves_one_space_behind() {
        assert_eq!(strip("Hello 👋 world"), "Hello world");
    }

    #[test]
    fn an_emoji_at_the_end_does_not_leave_a_trailing_space() {
        assert_eq!(strip("The lights are on 💡"), "The lights are on");
    }

    #[test]
    fn a_joined_sequence_is_removed_whole() {
        assert_eq!(strip("Family 👨‍👩‍👧 time"), "Family time");
    }

    #[test]
    fn a_flag_is_removed_whole() {
        assert_eq!(strip("Weather in 🇺🇸 today"), "Weather in today");
    }

    #[test]
    fn a_keycap_leaves_no_stray_digit_marker() {
        assert_eq!(strip("Option 1️⃣ selected"), "Option 1 selected");
    }

    #[test]
    fn words_and_punctuation_survive() {
        assert_eq!(strip("It's 72°F — warm, isn't it?"), "It's 72°F — warm, isn't it?");
    }

    #[test]
    fn currency_and_arithmetic_survive_because_a_voice_reads_them() {
        assert_eq!(strip("$5 + £3 = €9"), "$5 + £3 = €9");
    }

    #[test]
    fn a_segment_that_was_only_an_emoji_becomes_empty() {
        assert_eq!(strip("🎉"), "");
    }
}
