//! Splitting streamed model output into speakable units.
//!
//! Synthesis works on whole sentences, but a model produces text a token at a
//! time. This module pulls complete sentences out of a growing buffer so the
//! assistant can start speaking the first one while the rest is still being
//! generated.

/// Removes every complete sentence from the front of `buffer`, leaving the
/// unfinished remainder behind.
///
/// A sentence ends at `.`, `!`, or `?` *followed by whitespace*. Requiring the
/// whitespace is what keeps `3.14` and `e.g. this` in one piece; the trailing
/// fragment is flushed by the caller when the model finishes.
pub fn take_complete(buffer: &mut String) -> Vec<String> {
    let mut sentences = Vec::new();
    while let Some(end) = boundary(buffer) {
        let sentence: String = buffer.drain(..end).collect();
        let sentence = sentence.trim();
        if !sentence.is_empty() {
            sentences.push(sentence.to_owned());
        }
    }
    sentences
}

/// Byte offset just past the first sentence-ending punctuation run that is
/// followed by whitespace.
fn boundary(text: &str) -> Option<usize> {
    for (offset, character) in text.char_indices() {
        if !is_terminator(character) {
            continue;
        }

        // Treat runs like `?!` as one ending.
        let mut end = offset + character.len_utf8();
        while let Some(next) = text[end..].chars().next() {
            if is_terminator(next) {
                end += next.len_utf8();
            } else {
                break;
            }
        }

        if text[end..].chars().next().is_some_and(char::is_whitespace) {
            return Some(end);
        }
    }
    None
}

/// Whether `character` can end a sentence.
fn is_terminator(character: char) -> bool {
    matches!(character, '.' | '!' | '?')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(text: &str) -> (Vec<String>, String) {
        let mut buffer = text.to_owned();
        let sentences = take_complete(&mut buffer);
        (sentences, buffer)
    }

    #[test]
    fn a_complete_sentence_is_taken_and_the_rest_left_behind() {
        let (sentences, rest) = take("One. Two");
        assert_eq!(sentences, ["One."]);
        assert_eq!(rest, " Two");
    }

    #[test]
    fn several_sentences_come_out_in_order() {
        let (sentences, rest) = take("One. Two! Three? ");
        assert_eq!(sentences, ["One.", "Two!", "Three?"]);
        assert_eq!(rest, " ");
    }

    #[test]
    fn an_unfinished_sentence_is_held_back() {
        let (sentences, rest) = take("still going");
        assert!(sentences.is_empty());
        assert_eq!(rest, "still going");
    }

    #[test]
    fn a_trailing_terminator_waits_for_what_follows() {
        // The model may be about to emit `5` in `3.` — only whitespace proves
        // the sentence ended.
        let (sentences, rest) = take("The answer is 3.");
        assert!(sentences.is_empty());
        assert_eq!(rest, "The answer is 3.");
    }

    #[test]
    fn decimals_do_not_end_a_sentence() {
        let (sentences, rest) = take("It is 3.14 exactly");
        assert!(sentences.is_empty());
        assert_eq!(rest, "It is 3.14 exactly");
    }

    #[test]
    fn punctuation_runs_stay_together() {
        let (sentences, _) = take("Really?! Yes");
        assert_eq!(sentences, ["Really?!"]);
    }

    #[test]
    fn multibyte_text_splits_on_character_boundaries() {
        let (sentences, rest) = take("Ich hörte „ja“. Und dann");
        assert_eq!(sentences, ["Ich hörte „ja“."]);
        assert_eq!(rest, " Und dann");
    }
}
