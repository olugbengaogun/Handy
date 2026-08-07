//! Mining candidate dictionary terms from the user's own transcripts.
//!
//! The learning loop in [`crate::managers::learning`] is reactive: it needs the
//! user to make the same correction twice before it can offer anything. That is
//! the right default, but it means a brand-new install knows nothing, and the
//! terms most worth knowing — the names of the people, products and projects
//! someone talks about every day — are exactly the ones the model gets wrong
//! from the very first dictation.
//!
//! The history database already contains a large corpus of the user's own
//! language. Recurring proper nouns in it are a good prior for "words this
//! person says a lot", and feeding those to the decoder as a prompt biases it
//! toward recognising them.
//!
//! ## Everything here is a *suggestion*
//!
//! These terms come from model output, so a term the model consistently
//! mis-hears will be mined in its mis-heard form. Adding that automatically
//! would teach the app to reinforce its own error — the exact failure this
//! codebase works to avoid elsewhere. So nothing mined here is ever applied
//! without the user confirming it; the extraction only decides what is worth
//! *asking* about.

use std::collections::HashMap;

/// Minimum times a term must appear before it is worth suggesting.
const MIN_OCCURRENCES: usize = 3;

/// Longest phrase considered, in words. Beyond this it is a sentence fragment,
/// not a name.
const MAX_PHRASE_WORDS: usize = 3;

/// Shortest acceptable term. Two-letter capitalised tokens are overwhelmingly
/// sentence-initial words ("It", "In"), not names.
const MIN_TERM_CHARS: usize = 3;

/// Words that are capitalised constantly without being names. Kept deliberately
/// small — this is a precision filter for the most common false positives, not
/// an attempt at a stopword list.
const COMMON_CAPITALISED: &[&str] = &[
    "the", "this", "that", "these", "those", "there", "then", "they", "their", "them", "it", "its",
    "is", "in", "on", "at", "to", "for", "and", "but", "or", "so", "if", "we", "you", "your", "i",
    "he", "she", "his", "her", "a", "an", "as", "of", "my", "me", "was", "were", "be", "been",
    "have", "has", "had", "do", "does", "did", "not", "no", "yes", "what", "when", "where", "who",
    "why", "how", "can", "could", "would", "should", "will", "just", "like", "well", "okay", "ok",
    "let", "now", "here", "some", "all", "one", "two", "three", "first", "next", "also", "because",
    "actually", "really", "very", "right", "good", "great", "thanks", "thank", "please", "hello",
    "hi", "hey", "yeah", "sure", "maybe", "about", "with", "from", "by", "up", "out", "get", "got",
    "going", "want", "need", "know", "think", "see", "look", "make", "made", "take", "come",
];

/// A term mined from the corpus, with how often it was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermCandidate {
    pub term: String,
    pub occurrences: usize,
}

/// Strip surrounding punctuation, returning the alphanumeric core.
fn core(word: &str) -> &str {
    let start = word
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, _)| i);
    match start {
        None => "",
        Some(s) => {
            let e = word
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_alphanumeric())
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(s);
            &word[s..e]
        }
    }
}

/// Does this token look like part of a name rather than ordinary prose?
fn looks_like_name_part(word: &str) -> bool {
    let c = core(word);
    if c.chars().count() < MIN_TERM_CHARS {
        return false;
    }
    // Must start with an uppercase letter.
    if !c.chars().next().is_some_and(|ch| ch.is_uppercase()) {
        return false;
    }
    // Purely numeric tokens are not names.
    if c.chars().all(|ch| ch.is_numeric()) {
        return false;
    }
    let lower = c.to_lowercase();
    if COMMON_CAPITALISED.contains(&lower.as_str()) {
        return false;
    }
    // A contraction of a common word is still that common word. `core` only
    // strips punctuation from the *ends*, so "I'm" arrives intact, and the
    // stop-list holds "i" rather than every contraction of it — which is how
    // "I'm" came to be offered as a name to add to the dictionary, 73 sightings
    // deep.
    //
    // Matching on the stem rather than listing contractions covers I'm/I'll/
    // I've/I'd, we're, they've, it's, that's, don't and the rest at once, in
    // one rule instead of forty. Real names keep their apostrophes: the stem of
    // "O'Brien" is "o" and of "D'Angelo" is "d", neither of which is a common
    // word, so both still qualify.
    //
    // Both apostrophes are handled: speech-to-text emits the typographic U+2019
    // at least as often as the ASCII one.
    if let Some((stem, _)) = lower.split_once(['\'', '\u{2019}']) {
        if COMMON_CAPITALISED.contains(&stem) {
            return false;
        }
    }
    true
}

/// Is this token the first word of a sentence?
///
/// Sentence-initial capitalisation carries no information about whether a word
/// is a name, so those positions are skipped entirely. Without this the mining
/// output is dominated by whatever word the user happens to start sentences
/// with.
fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(|c: char| !c.is_alphanumeric()).len() < word.trim_end().len()
        && word.ends_with(['.', '!', '?', ':', ';'])
}

/// Mine recurring capitalised terms and phrases from a corpus of transcripts.
///
/// Returns candidates ordered by frequency (most frequent first), excluding
/// anything already in `existing`. Longer phrases are preferred over their own
/// constituent words: if "Handy Plus" recurs, "Handy" alone is not also offered.
pub fn mine_candidates(transcripts: &[String], existing: &[String]) -> Vec<TermCandidate> {
    let known: Vec<String> = existing.iter().map(|w| w.to_lowercase()).collect();
    let mut counts: HashMap<String, usize> = HashMap::new();

    for transcript in transcripts {
        let words: Vec<&str> = transcript.split_whitespace().collect();

        // `sentence_start` tracks whether the *next* token opens a sentence.
        let mut sentence_start = true;

        for (i, word) in words.iter().enumerate() {
            let is_start = sentence_start;
            sentence_start = ends_sentence(word);

            if is_start || !looks_like_name_part(word) {
                continue;
            }

            // Grow the longest run of name-like tokens starting here.
            //
            // The sentence check happens *before* extending, not after adding:
            // in "call Beverly. Carmack will know", "Beverly." closes its
            // sentence, so the phrase must end there. Checking afterwards would
            // have already swallowed "Carmack" and invented the phrase
            // "Beverly Carmack", which nobody ever said.
            let mut phrase: Vec<&str> = vec![core(word)];
            if !ends_sentence(word) {
                for next in words.iter().skip(i + 1).take(MAX_PHRASE_WORDS - 1) {
                    if !looks_like_name_part(next) {
                        break;
                    }
                    phrase.push(core(next));
                    if ends_sentence(next) {
                        break;
                    }
                }
            }

            *counts.entry(phrase.join(" ")).or_insert(0) += 1;
        }
    }

    let mut candidates: Vec<TermCandidate> = counts
        .into_iter()
        .filter(|(term, count)| *count >= MIN_OCCURRENCES && !known.contains(&term.to_lowercase()))
        .map(|(term, occurrences)| TermCandidate { term, occurrences })
        .collect();

    // Frequency first, then alphabetically so the ordering is stable across runs
    // rather than depending on hash iteration order.
    candidates.sort_by(|a, b| {
        b.occurrences
            .cmp(&a.occurrences)
            .then_with(|| a.term.cmp(&b.term))
    });

    // Drop any single word that is already covered by a longer phrase that made
    // the cut — suggesting both "Handy" and "Handy Plus" is noise.
    let phrases: Vec<String> = candidates
        .iter()
        .filter(|c| c.term.contains(' '))
        .map(|c| c.term.to_lowercase())
        .collect();

    candidates.retain(|candidate| {
        candidate.term.contains(' ')
            || !phrases.iter().any(|phrase| {
                phrase
                    .split(' ')
                    .any(|part| part == candidate.term.to_lowercase())
            })
    });

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    /// "I'm" was being offered as a name to add to the dictionary. `core` only
    /// strips punctuation from the ends, so the contraction reached the
    /// stop-list check intact and "i'm" is not "i".
    #[test]
    fn contractions_of_common_words_are_not_names() {
        for word in ["I'm", "I've", "It's", "That's", "They're", "Don't"] {
            assert!(
                !looks_like_name_part(word),
                "{word} should not be mined as a name"
            );
        }
    }

    /// The typographic apostrophe speech-to-text actually emits.
    #[test]
    fn curly_apostrophes_are_handled_too() {
        assert!(!looks_like_name_part("I\u{2019}m"));
        assert!(!looks_like_name_part("They\u{2019}re"));
    }

    /// Matching on the stem must not cost real names their apostrophes.
    #[test]
    fn apostrophes_in_real_names_survive() {
        assert!(looks_like_name_part("O'Brien"));
        assert!(looks_like_name_part("D'Angelo"));
    }

    /// The end-to-end shape of the bug: a transcript full of contractions
    /// should mine the name and nothing else.
    #[test]
    fn mining_ignores_contractions_but_keeps_the_name() {
        let terms = mine_candidates(
            &corpus(&[
                "I'm talking to Chidimma. I'm sure I've met Chidimma before.",
                "I'm certain that's Chidimma.",
            ]),
            &[],
        );
        let mined: Vec<&str> = terms.iter().map(|t| t.term.as_str()).collect();
        assert!(mined.contains(&"Chidimma"), "got {mined:?}");
        assert!(
            !mined.iter().any(|t| t.contains('\'')),
            "no contraction should be offered: {mined:?}"
        );
    }

    fn terms(candidates: &[TermCandidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.term.as_str()).collect()
    }

    #[test]
    fn an_empty_corpus_yields_nothing() {
        assert!(mine_candidates(&[], &[]).is_empty());
    }

    #[test]
    fn a_recurring_name_is_mined() {
        let c = corpus(&[
            "I spoke to Beverly about it.",
            "Ask Beverly for the file.",
            "Beverly is handling that.",
            "Send it to Beverly please.",
        ]);
        // The sentence-initial "Beverly" is skipped; the other three count.
        assert!(terms(&mine_candidates(&c, &[])).contains(&"Beverly"));
    }

    #[test]
    fn a_rare_name_is_not_mined() {
        let c = corpus(&["I met Zbigniew once.", "Nothing else here."]);
        assert!(mine_candidates(&c, &[]).is_empty());
    }

    #[test]
    fn sentence_initial_words_are_not_mistaken_for_names() {
        // "Actually" opens every sentence; it must not become a dictionary term.
        let c = corpus(&[
            "Actually that is fine.",
            "Actually I disagree.",
            "Actually we should wait.",
            "Actually no.",
        ]);
        assert!(
            mine_candidates(&c, &[]).is_empty(),
            "mined: {:?}",
            mine_candidates(&c, &[])
        );
    }

    #[test]
    fn common_capitalised_words_are_filtered() {
        let c = corpus(&[
            "yes The Thing is here",
            "no The Thing again",
            "see The Thing once more",
            "and The Thing yet again",
        ]);
        let candidates = mine_candidates(&c, &[]);
        let mined = terms(&candidates);
        assert!(!mined.iter().any(|t| t.starts_with("The")), "{mined:?}");
    }

    #[test]
    fn a_multi_word_name_is_kept_whole() {
        let c = corpus(&[
            "we use Handy Plus daily",
            "I like Handy Plus a lot",
            "try Handy Plus today",
            "Handy Plus is good",
        ]);
        assert!(terms(&mine_candidates(&c, &[])).contains(&"Handy Plus"));
    }

    #[test]
    fn a_word_already_covered_by_a_phrase_is_not_offered_separately() {
        let c = corpus(&[
            "we use Handy Plus daily",
            "I like Handy Plus a lot",
            "try Handy Plus today",
        ]);
        let candidates = mine_candidates(&c, &[]);
        let mined = terms(&candidates);
        assert!(mined.contains(&"Handy Plus"));
        assert!(
            !mined.contains(&"Handy"),
            "redundant single word: {mined:?}"
        );
    }

    #[test]
    fn terms_already_in_the_dictionary_are_excluded() {
        let c = corpus(&[
            "ask Beverly now",
            "ask Beverly again",
            "ask Beverly once more",
        ]);
        let existing = vec!["beverly".to_string()];
        assert!(mine_candidates(&c, &existing).is_empty());
    }

    #[test]
    fn short_and_numeric_tokens_are_rejected() {
        let c = corpus(&["it is X1 here", "it is X1 there", "it is X1 again"]);
        let candidates = mine_candidates(&c, &[]);
        let mined = terms(&candidates);
        assert!(!mined.contains(&"X1"), "{mined:?}");
    }

    #[test]
    fn results_are_ordered_by_frequency_then_alphabetically() {
        let c = corpus(&[
            "ask Beverly and Carmack",
            "ask Beverly and Carmack",
            "ask Beverly and Carmack",
            "ask Beverly now",
        ]);
        let mined = mine_candidates(&c, &[]);
        // Deterministic ordering matters: hash iteration order would otherwise
        // shuffle the suggestion list between runs.
        assert!(mined
            .windows(2)
            .all(|w| w[0].occurrences >= w[1].occurrences));
    }

    #[test]
    fn punctuation_around_names_is_stripped() {
        let c = corpus(&[
            "we asked (Beverly) about it",
            "we asked Beverly, again",
            "we asked Beverly.",
        ]);
        assert!(terms(&mine_candidates(&c, &[])).contains(&"Beverly"));
    }

    #[test]
    fn a_phrase_does_not_run_across_a_full_stop() {
        let c = corpus(&[
            "call Beverly. Carmack will know",
            "call Beverly. Carmack will know",
            "call Beverly. Carmack will know",
        ]);
        let candidates = mine_candidates(&c, &[]);
        let mined = terms(&candidates);
        assert!(
            !mined.iter().any(|t| t.contains("Beverly Carmack")),
            "phrase crossed a sentence boundary: {mined:?}"
        );
    }
}
