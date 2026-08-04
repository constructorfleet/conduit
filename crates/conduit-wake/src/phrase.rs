//! Turning model file names into phrases and back.

/// The phrase a model file listens for.
///
/// openWakeWord names its models after the phrase and a version —
/// `hey_jarvis_v0.1.onnx` — because a phrase can be retrained without becoming
/// a different phrase. An operator asked to pick from a list should see
/// `hey jarvis`, and a pipeline that names `hey jarvis` should find the model
/// whatever version it was trained at.
#[must_use]
pub fn phrase_from_model_name(stem: &str) -> String {
    let base = strip_version(stem);
    base.replace('_', " ").trim().to_owned()
}

/// Removes a trailing `_v<major>.<minor>` from a model file stem.
fn strip_version(stem: &str) -> &str {
    let Some((base, version)) = stem.rsplit_once("_v") else {
        return stem;
    };
    let is_version = !version.is_empty()
        && version.chars().all(|character| character.is_ascii_digit() || character == '.');
    if is_version {
        base
    } else {
        stem
    }
}

/// Whether a phrase names this model, ignoring case and separators.
///
/// A pipeline may have been written against the file name, the phrase, or
/// either one capitalized; all three mean the same detector.
#[must_use]
pub fn phrase_matches(phrase: &str, model_phrase: &str) -> bool {
    normalize(phrase) == normalize(model_phrase)
}

fn normalize(phrase: &str) -> String {
    phrase
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_file_names_the_phrase_it_listens_for() {
        assert_eq!(phrase_from_model_name("hey_jarvis_v0.1"), "hey jarvis");
        assert_eq!(phrase_from_model_name("alexa_v0.1"), "alexa");
        assert_eq!(phrase_from_model_name("hey_mycroft"), "hey mycroft");
    }

    #[test]
    fn a_version_is_only_stripped_when_it_is_one() {
        // `_v` is not a version marker when what follows it is a word: a model
        // named for a phrase ending in one keeps it.
        assert_eq!(phrase_from_model_name("okay_victor"), "okay victor");
        assert_eq!(phrase_from_model_name("hey_jarvis_v2"), "hey jarvis");
    }

    #[test]
    fn a_phrase_matches_however_it_was_written() {
        assert!(phrase_matches("hey jarvis", "hey jarvis"));
        assert!(phrase_matches("Hey Jarvis", "hey jarvis"));
        assert!(phrase_matches("hey_jarvis", "hey jarvis"));
        assert!(!phrase_matches("hey mycroft", "hey jarvis"));
    }
}
