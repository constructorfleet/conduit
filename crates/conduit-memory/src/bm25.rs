//! BM25 over unigrams, the ranking both backends share.
//!
//! Okapi BM25 with the usual `k1 = 1.2` and `b = 0.75`, over lowercased
//! alphanumeric unigrams. There is no stemmer and no stopword list, because
//! both are language assumptions: nothing in a [`Record`] says what language
//! the transcript was in, and an English stopword list applied to a German
//! transcript throws away content words. Not stemming costs recall on
//! inflections; guessing wrong costs correctness, and this is the ranking a
//! degraded store falls back to.
//!
//! BM25 is unbounded, so [`rank`] divides by the best score in the result set.
//! That is legitimate precisely because [`Match::score`] is documented as
//! "comparable only within one result set": a caller may not compare a score
//! from one search to a score from another, so normalising per search does not
//! break any promise it was allowed to rely on. The consequence is that the
//! best match always scores exactly `1.0`.
//!
//! [`Record`]: conduit_provider::memory::Record
//! [`Match::score`]: conduit_provider::memory::Match::score

use std::collections::{BTreeSet, HashMap};

/// Term frequency saturation. Higher means repeated terms keep counting.
const K1: f32 = 1.2;

/// Length normalisation. Higher penalises long documents more.
const B: f32 = 0.75;

/// Splits `text` into the tokens this ranking counts.
///
/// Lowercased and split on anything that is not alphanumeric, dropping
/// single-character tokens: a lone letter matches too much to be evidence of
/// anything, and "a" and "I" are the two most common one-character tokens in
/// English while carrying no meaning worth retrieving on.
#[must_use]
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
}

/// Ranks `documents` against `query`, best first, keeping at most `limit`.
///
/// `documents` must be ordered most recent first. The sort is stable, so two
/// documents scoring the same keep that order — which is how ties break by
/// recency without recency entering the score.
///
/// Returns `(index, score)` pairs indexing back into `documents`, with scores
/// in `0.0..=1.0`. A document no query term appears in scores zero and is
/// dropped rather than returned: a caller asked for what matched, and a record
/// with score `0.0` in the list is a record that did not match dressed up as
/// one that did.
#[must_use]
pub fn rank<D: AsRef<[String]>>(
    documents: &[D],
    query: &str,
    limit: usize,
) -> Vec<(usize, f32)> {
    let terms: BTreeSet<String> = tokens(query).into_iter().collect();
    if terms.is_empty() || documents.is_empty() || limit == 0 {
        return Vec::new();
    }

    let total = documents.len() as f32;
    let lengths: Vec<f32> =
        documents.iter().map(|document| document.as_ref().len() as f32).collect();
    let average: f32 = lengths.iter().sum::<f32>() / total;
    if average <= 0.0 {
        // Every document tokenised to nothing, so nothing can match and the
        // length normalisation below would divide by zero.
        return Vec::new();
    }

    // Document frequency once per term rather than once per term per document:
    // the inner loop is otherwise quadratic in the candidate set, and the
    // candidate set is whatever the filters let through.
    let frequencies: HashMap<&String, f32> = terms
        .iter()
        .map(|term| {
            let containing =
                documents.iter().filter(|document| document.as_ref().contains(term)).count()
                    as f32;
            (term, containing)
        })
        .collect();

    let mut scored: Vec<(usize, f32)> = documents
        .iter()
        .enumerate()
        .filter_map(|(index, document)| {
            let score: f32 = terms
                .iter()
                .map(|term| {
                    let occurrences =
                        document.as_ref().iter().filter(|token| *token == term).count() as f32;
                    if occurrences <= 0.0 {
                        return 0.0;
                    }
                    let containing = frequencies.get(term).copied().unwrap_or(0.0);
                    // The `ln(1 + ...)` form rather than `ln(...)`, so a term
                    // in more than half the documents scores small rather than
                    // negative. A negative contribution would let a common
                    // term push a document that contains a rare query term
                    // *below* one that contains neither.
                    let idf = (1.0 + (total - containing + 0.5) / (containing + 0.5)).ln();
                    let saturation =
                        occurrences + K1 * (1.0 - B + B * lengths[index] / average);
                    idf * occurrences * (K1 + 1.0) / saturation
                })
                .sum();
            (score > 0.0).then_some((index, score))
        })
        .collect();

    scored.sort_by(|left, right| right.1.total_cmp(&left.1));
    scored.truncate(limit);

    // Truncating first, so the divisor is the same either way: the best score
    // survives any truncation.
    let best = scored.first().map_or(1.0, |(_, score)| *score);
    scored.into_iter().map(|(index, score)| (index, (score / best).clamp(0.0, 1.0))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokenises each document, most recent first, as [`rank`] expects.
    fn documents(texts: &[&str]) -> Vec<Vec<String>> {
        texts.iter().map(|text| tokens(text)).collect()
    }

    #[test]
    fn tokens_are_lowercased_and_split_on_anything_that_is_not_a_letter_or_digit() {
        assert_eq!(
            tokens("Turn ON the kitchen-light, please!"),
            ["turn", "on", "the", "kitchen", "light", "please"]
        );
    }

    #[test]
    fn a_single_character_token_is_dropped_because_it_matches_too_much() {
        assert_eq!(tokens("I have a cat"), ["have", "cat"]);
    }

    #[test]
    fn nothing_is_stemmed_because_nothing_here_knows_the_language() {
        // "running" and "runs" stay distinct. Losing that recall is the price
        // of not assuming English.
        assert_eq!(tokens("running runs"), ["running", "runs"]);
    }

    #[test]
    fn a_relevant_document_ranks_above_an_irrelevant_one() {
        let corpus =
            documents(&["the recycling goes out on tuesday", "the cat is called mabel"]);
        let ranked = rank(&corpus, "when does the recycling go out", 5);

        assert_eq!(ranked.first().map(|(index, _)| *index), Some(0), "{ranked:?}");
    }

    #[test]
    fn a_document_no_query_term_appears_in_is_dropped_rather_than_scored_zero() {
        let corpus = documents(&["the recycling goes out on tuesday", "unrelated"]);
        let ranked = rank(&corpus, "recycling", 5);

        assert_eq!(ranked.len(), 1, "only the matching document is returned: {ranked:?}");
        assert_eq!(ranked[0].0, 0);
    }

    #[test]
    fn every_score_lands_between_zero_and_one() {
        let corpus = documents(&[
            "recycling day is tuesday",
            "recycling is collected fortnightly in this street",
            "the bins are green",
        ]);
        let ranked = rank(&corpus, "recycling collected", 5);

        assert!(!ranked.is_empty());
        for (index, score) in &ranked {
            assert!((0.0..=1.0).contains(score), "document {index} scored {score}");
        }
    }

    #[test]
    fn the_best_match_scores_exactly_one_because_the_set_is_normalised_by_it() {
        let corpus = documents(&["recycling day is tuesday", "recycling"]);
        let ranked = rank(&corpus, "recycling day", 5);

        assert_eq!(ranked.first().map(|(_, score)| *score), Some(1.0), "{ranked:?}");
    }

    #[test]
    fn documents_that_score_the_same_keep_the_order_they_arrived_in() {
        // Callers pass the most recent first, so a stable sort is what makes
        // ties break by recency.
        let corpus = documents(&["recycling tuesday", "recycling tuesday"]);
        let ranked = rank(&corpus, "recycling tuesday", 5);

        assert_eq!(ranked.iter().map(|(index, _)| *index).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn a_shorter_document_wins_when_both_carry_the_query_term_once() {
        // The `b` term is what makes this true, and it is why a store that
        // concatenates a question and an answer into one document ranks
        // differently from one that stores them apart.
        let corpus = documents(&[
            "recycling and a great deal of other text that dilutes the term entirely",
            "recycling tuesday",
        ]);
        let ranked = rank(&corpus, "recycling", 5);

        assert_eq!(ranked.first().map(|(index, _)| *index), Some(1), "{ranked:?}");
    }

    #[test]
    fn a_common_word_carries_almost_no_weight_which_is_what_replaces_a_stopword_list() {
        // The inverse document frequency does the job a stopword list would,
        // without assuming a language: a term in every document contributes
        // almost nothing, so "the" cannot outrank "recycling".
        let corpus = documents(&[
            "the recycling goes out on tuesday",
            "the cat is called mabel",
            "the dentist is on thursday",
        ]);
        let ranked = rank(&corpus, "the recycling", 5);

        assert_eq!(ranked.first().map(|(index, _)| *index), Some(0), "{ranked:?}");
    }

    #[test]
    fn a_rare_word_outranks_the_word_the_caller_cared_about_in_a_tiny_corpus() {
        // The honest limitation of having no stopword list. With two documents,
        // "is" appears in one and so looks *rare*, which outweighs "recycling"
        // appearing in the other. It corrects itself as the corpus grows — see
        // the test above — but a two-record store ranks by surprise rather than
        // by aboutness, and that is worth knowing rather than discovering.
        let corpus =
            documents(&["the recycling goes out on tuesday", "the cat is called mabel"]);
        let ranked = rank(&corpus, "when is the recycling collected", 5);

        assert_eq!(
            ranked.first().map(|(index, _)| *index),
            Some(1),
            "documented, not desired: {ranked:?}"
        );
    }

    #[test]
    fn a_limit_of_zero_returns_nothing_rather_than_everything() {
        let corpus = documents(&["recycling day is tuesday"]);
        assert!(rank(&corpus, "recycling", 0).is_empty());
    }

    #[test]
    fn an_empty_corpus_and_an_empty_query_both_rank_nothing() {
        assert!(rank(&documents(&[]), "recycling", 5).is_empty());
        assert!(rank(&documents(&["recycling"]), "", 5).is_empty());
        assert!(rank(&documents(&["recycling"]), "a !", 5).is_empty());
    }

    #[test]
    fn documents_that_tokenise_to_nothing_do_not_divide_by_zero() {
        // An all-punctuation corpus has an average length of zero, which the
        // length normalisation would otherwise divide by.
        let corpus = documents(&["!!!", "?"]);
        assert!(rank(&corpus, "recycling", 5).is_empty());
    }
}
