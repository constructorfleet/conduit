//! Turning the markdown a model writes into words a voice can say.
//!
//! A model formats because it is usually read on a screen. Spoken, that
//! formatting is at best noise — "asterisk asterisk important asterisk
//! asterisk" — and at worst a URL read out one character at a time. What is
//! removed here is the notation; what is kept is every word the notation was
//! wrapped around.
//!
//! This is deliberately not a markdown parser. It sees one sentence at a time,
//! because synthesis starts before the model has finished writing, so a
//! construct spanning several sentences — most obviously a fenced code block —
//! cannot be recognised as one thing. Each line is judged on its own, which is
//! what makes the result predictable on a fragment.

use crate::whitespace::collapse;

/// Rewrites the markdown in `text` as plain speakable words.
#[must_use]
pub fn flatten(text: &str) -> String {
    let spoken: Vec<String> = text.lines().filter_map(speakable_line).collect();
    collapse(&spoken.join(" "))
}

/// One line's spoken form, or `None` when the line is pure notation.
fn speakable_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_fence(trimmed) || is_thematic_break(trimmed) {
        return None;
    }
    if is_table_divider(trimmed) {
        return None;
    }

    let body = if is_table_row(trimmed) { table_row(trimmed) } else { block_body(trimmed) };
    let spoken = inline(&body);
    (!spoken.trim().is_empty()).then_some(spoken)
}

/// Whether the line opens or closes a fenced code block.
fn is_fence(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

/// Whether the line is a horizontal rule.
fn is_thematic_break(line: &str) -> bool {
    let marks: Vec<char> =
        line.chars().filter(|character| !character.is_whitespace()).collect();
    marks.len() >= 3
        && matches!(marks[0], '-' | '*' | '_')
        && marks.iter().all(|character| *character == marks[0])
}

/// Whether the line is the `|---|:--:|` rule under a table's header.
fn is_table_divider(line: &str) -> bool {
    line.contains('|')
        && line.contains('-')
        && line.chars().all(|character| matches!(character, '|' | '-' | ':' | ' ' | '\t'))
}

/// Whether the line is a row of a table.
fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.len() > 1
}

/// A table row as a spoken list of its cells.
///
/// The pipes become commas because that is what the columns are: a listener
/// hearing "eight, cloudy, twelve" can follow a row, and one hearing "pipe
/// eight pipe cloudy pipe" cannot.
fn table_row(line: &str) -> String {
    let cells: Vec<&str> = line
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect();
    cells.join(", ")
}

/// The line with its leading block notation removed.
///
/// Quotes, list markers and headings nest — `> - **Note**` is all three — so
/// they are stripped in a loop rather than once.
fn block_body(line: &str) -> String {
    let mut rest = line;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('>') {
            rest = after;
            continue;
        }
        if let Some(after) = strip_list_marker(rest) {
            rest = after;
            continue;
        }
        if let Some(after) = strip_heading(rest) {
            rest = after;
            continue;
        }
        if let Some(after) = strip_checkbox(rest) {
            rest = after;
            continue;
        }
        break;
    }
    rest.to_owned()
}

/// The text after a bullet or number, when the line starts with one.
///
/// The marker must be followed by whitespace, which is what tells `- item`
/// from `-5 degrees` and `1. First` from `3.14`.
fn strip_list_marker(line: &str) -> Option<&str> {
    let after_bullet = line
        .strip_prefix('-')
        .or_else(|| line.strip_prefix('*'))
        .or_else(|| line.strip_prefix('+'));
    if let Some(rest) = after_bullet {
        return rest.starts_with(char::is_whitespace).then(|| rest.trim_start());
    }

    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    rest.starts_with(char::is_whitespace).then(|| rest.trim_start())
}

/// The text of a heading, when the line is one.
///
/// The hashes must be followed by whitespace, so a `#hashtag` keeps its hash.
fn strip_heading(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches('#');
    if rest.len() == line.len() || !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim_start().trim_end_matches('#').trim_end())
}

/// The text after a task list's checkbox, when the line starts with one.
fn strip_checkbox(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("[ ]")
        .or_else(|| line.strip_prefix("[x]"))
        .or_else(|| line.strip_prefix("[X]"))?;
    Some(rest.trim_start())
}

/// The line with its inline notation removed.
fn inline(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut spoken = String::with_capacity(text.len());
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        let advanced = match character {
            '\\' => escape(&characters, index, &mut spoken),
            '`' => code_span(&characters, index, &mut spoken),
            '!' if characters.get(index + 1) == Some(&'[') => {
                link(&characters, index + 1, &mut spoken)
            }
            '[' => link(&characters, index, &mut spoken),
            '<' => autolink(&characters, index),
            '*' | '~' | '_' => emphasis(&characters, index),
            _ => None,
        };

        match advanced {
            Some(next) => index = next,
            None => {
                spoken.push(character);
                index += 1;
            }
        }
    }

    spoken
}

/// Emits the character a backslash escapes, and reports where to resume.
fn escape(characters: &[char], start: usize, spoken: &mut String) -> Option<usize> {
    let escaped = *characters.get(start + 1)?;
    if escaped.is_alphanumeric() {
        return None;
    }
    spoken.push(escaped);
    Some(start + 2)
}

/// Emits the contents of a code span, and reports where to resume.
///
/// Code is read aloud because it is usually the answer to the question — a
/// file name, a command, an entity id. Only the backticks go.
fn code_span(characters: &[char], start: usize, spoken: &mut String) -> Option<usize> {
    let opening = run_length(characters, start, '`');
    let body = start + opening;
    let mut index = body;
    while index < characters.len() {
        if characters[index] == '`' && run_length(characters, index, '`') == opening {
            spoken.extend(&characters[body..index]);
            return Some(index + opening);
        }
        index += 1;
    }
    // An unclosed span is a sentence that ends mid-code, which is ordinary
    // when the model is still writing. The words are still words.
    spoken.extend(&characters[body..]);
    Some(characters.len())
}

/// Emits a link's or image's text, and reports where to resume.
fn link(characters: &[char], start: usize, spoken: &mut String) -> Option<usize> {
    let text_end = matching(characters, start, '[', ']')?;
    let after = *characters.get(text_end + 1)?;
    let end = match after {
        '(' => matching(characters, text_end + 1, '(', ')')?,
        '[' => matching(characters, text_end + 1, '[', ']')?,
        // A bare `[text]` is not a link, it is brackets.
        _ => return None,
    };

    let text: String = characters[start + 1..text_end].iter().collect();
    spoken.push_str(&inline(&text));
    Some(end + 1)
}

/// Reports where to resume after an autolink, which has no spoken form.
///
/// `<https://example.com/a/b?c=d>` read aloud is a minute of punctuation. The
/// sentence around it stands on its own, so the address is dropped rather than
/// announced.
fn autolink(characters: &[char], start: usize) -> Option<usize> {
    let end = matching(characters, start, '<', '>')?;
    let body: String = characters[start + 1..end].iter().collect();
    let is_address = body.contains("://") || (body.contains('@') && !body.contains(' '));
    is_address.then_some(end + 1)
}

/// Reports where to resume after an emphasis marker, when this is one.
///
/// A marker hugs the text it emphasises: `**bold**` opens against a word and
/// closes against one. `2 * 3` has space on both sides, so it is arithmetic
/// and stays, and `snake_case` has letters on both sides, so it stays too.
fn emphasis(characters: &[char], start: usize) -> Option<usize> {
    let marker = characters[start];
    let length = run_length(characters, start, marker);
    let before = start.checked_sub(1).map(|index| characters[index]);
    let after = characters.get(start + length).copied();

    let opens = after.is_some_and(|next| !next.is_whitespace());
    let closes = before.is_some_and(|previous| !previous.is_whitespace());
    if !opens && !closes {
        return None;
    }

    // An underscore inside a word is part of the word.
    if marker == '_'
        && (before.is_some_and(char::is_alphanumeric)
            && after.is_some_and(char::is_alphanumeric))
    {
        return None;
    }

    Some(start + length)
}

/// How many times `marker` repeats starting at `start`.
fn run_length(characters: &[char], start: usize, marker: char) -> usize {
    characters[start..].iter().take_while(|character| **character == marker).count()
}

/// Index of the `close` that matches the `open` at `start`, honouring nesting.
fn matching(characters: &[char], start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0_usize;
    for (offset, character) in characters[start..].iter().enumerate() {
        if *character == open {
            depth += 1;
        } else if *character == close {
            depth -= 1;
            if depth == 0 {
                return Some(start + offset);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_and_italic_markers_go_and_the_words_stay() {
        assert_eq!(
            flatten("That is **very** _important_ indeed"),
            "That is very important indeed"
        );
    }

    #[test]
    fn strikethrough_markers_go() {
        assert_eq!(
            flatten("The meeting is ~~cancelled~~ moved"),
            "The meeting is cancelled moved"
        );
    }

    #[test]
    fn multiplication_is_not_emphasis() {
        assert_eq!(flatten("That is 2 * 3 watts"), "That is 2 * 3 watts");
    }

    #[test]
    fn an_underscore_inside_a_word_stays() {
        assert_eq!(flatten("Set light_level to 40"), "Set light_level to 40");
    }

    #[test]
    fn a_heading_reads_as_its_text() {
        assert_eq!(flatten("## Today's forecast"), "Today's forecast");
    }

    #[test]
    fn a_hashtag_is_not_a_heading() {
        assert_eq!(flatten("Filed under #home"), "Filed under #home");
    }

    #[test]
    fn a_bullet_reads_as_its_item() {
        assert_eq!(flatten("- Turn off the porch light"), "Turn off the porch light");
    }

    #[test]
    fn a_negative_number_is_not_a_bullet() {
        assert_eq!(flatten("-5 degrees outside"), "-5 degrees outside");
    }

    #[test]
    fn a_numbered_item_reads_without_its_number() {
        assert_eq!(flatten("1. Open the door"), "Open the door");
    }

    #[test]
    fn a_decimal_is_not_a_numbered_item() {
        assert_eq!(flatten("3.14 is close enough"), "3.14 is close enough");
    }

    #[test]
    fn a_quote_reads_as_what_was_quoted() {
        assert_eq!(flatten("> She said yes"), "She said yes");
    }

    #[test]
    fn nested_notation_is_stripped_to_the_words() {
        assert_eq!(flatten("> - **Note**: it is on"), "Note: it is on");
    }

    #[test]
    fn a_task_item_reads_without_its_checkbox() {
        assert_eq!(flatten("- [x] Take out the bins"), "Take out the bins");
    }

    #[test]
    fn a_link_reads_as_its_text_and_not_its_address() {
        assert_eq!(
            flatten("See [the forecast](https://example.com/weather?q=1) for details"),
            "See the forecast for details"
        );
    }

    #[test]
    fn an_image_reads_as_its_alt_text() {
        assert_eq!(flatten("Here is ![a chart](chart.png) for you"), "Here is a chart for you");
    }

    #[test]
    fn a_reference_link_reads_as_its_text() {
        assert_eq!(flatten("Read [the docs][docs] first"), "Read the docs first");
    }

    #[test]
    fn brackets_that_are_not_a_link_stay() {
        assert_eq!(flatten("The value is [unknown] today"), "The value is [unknown] today");
    }

    #[test]
    fn an_autolink_is_dropped_because_an_address_has_no_spoken_form() {
        assert_eq!(flatten("Details at <https://example.com/x> now"), "Details at now");
    }

    #[test]
    fn inline_code_reads_as_the_code() {
        assert_eq!(flatten("Run `systemctl restart` now"), "Run systemctl restart now");
    }

    #[test]
    fn an_unclosed_code_span_still_reads() {
        assert_eq!(flatten("Run `systemctl restart"), "Run systemctl restart");
    }

    #[test]
    fn a_fence_line_is_dropped_and_its_contents_are_not() {
        assert_eq!(flatten("```bash\nls -la\n```"), "ls -la");
    }

    #[test]
    fn a_horizontal_rule_is_dropped() {
        assert_eq!(flatten("Before\n---\nAfter"), "Before After");
    }

    #[test]
    fn a_table_reads_as_its_cells() {
        assert_eq!(
            flatten("| Time | Temp |\n|------|------|\n| 8am | 12 |"),
            "Time, Temp 8am, 12"
        );
    }

    #[test]
    fn an_escaped_marker_reads_as_the_character() {
        assert_eq!(flatten(r"A literal \* star"), "A literal * star");
    }

    #[test]
    fn plain_prose_is_left_alone() {
        assert_eq!(flatten("The porch light is on."), "The porch light is on.");
    }

    #[test]
    fn a_segment_of_pure_notation_becomes_empty() {
        assert_eq!(flatten("---"), "");
    }
}
