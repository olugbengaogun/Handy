//! Word-level diffing between a raw transcript and the user's edited version.
//!
//! This is the input side of the learning loop. When someone fixes a transcript
//! by hand, the difference between what the model produced and what they meant
//! is the single most valuable training signal the app has — and it needs no
//! audio, so it works with recording retention switched off.
//!
//! The previous implementation only recognised **one** shape of edit: an exact
//! single-word substitution where both versions had the same word count. That
//! misses multi-word names, insertions, deletions, capitalisation fixes and
//! punctuation fixes — which is to say, most real corrections.
//!
//! ## Why edits are typed
//!
//! Not every edit is an ASR error, and the ones that are do not all belong in
//! the same place:
//!
//! * A **vocabulary** fix ("andy plus" → "Handy Plus") belongs in the correction
//!   dictionary and the decoder prompt.
//! * A **style** fix (capitalisation, punctuation, "twenty five" → "25") belongs
//!   in the post-processing style profile. Putting it in a phonetic matcher
//!   would be nonsense — nothing was misheard.
//! * A **rewrite** is usually the user changing their mind about phrasing, not
//!   correcting a mistake. Promoting one of those to a rule actively corrupts
//!   future transcripts where the original wording was right. These are
//!   classified so they can be recorded and then *ignored*.
//!
//! Classification is what makes it safe to learn automatically at all.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// Maximum words on either side before diffing is abandoned.
///
/// The alignment below is O(n × m) in words. A pathological paste (an entire
/// document into the edit box) would otherwise stall the UI thread. Beyond this
/// the edit is simply not learned from — no correctness impact, only a missed
/// learning opportunity on an input that was never going to yield a clean rule.
const MAX_WORDS: usize = 2_000;

/// What kind of change an edit represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Different word(s) entirely — the vocabulary signal.
    Substitution,
    /// Words present in the edit but not the original.
    Insertion,
    /// Words present in the original but not the edit.
    Deletion,
    /// Same letters, different capitalisation ("iphone" → "iPhone").
    CasingOnly,
    /// Same word, different surrounding punctuation ("hello" → "hello,").
    PunctuationOnly,
    /// A spelled-out number became digits, or vice versa.
    NumberFormat,
    /// A large, semantically divergent change. Recorded for statistics and
    /// **never** auto-promoted to a rule.
    Rewrite,
}

impl EditKind {
    /// Whether this kind of edit may become an automatic correction rule.
    ///
    /// Insertions and deletions are excluded deliberately: a rule cannot be
    /// keyed on text that was not there, and "delete this word everywhere" is
    /// far too blunt to apply automatically.
    pub fn is_promotable(self) -> bool {
        matches!(
            self,
            EditKind::Substitution | EditKind::CasingOnly | EditKind::NumberFormat
        )
    }

    /// Whether this edit teaches vocabulary (goes to the dictionary) as opposed
    /// to style (goes to the post-processing profile).
    pub fn is_vocabulary(self) -> bool {
        matches!(self, EditKind::Substitution)
    }

    /// Stable identifier for persistence. Kept deliberately separate from the
    /// enum's Rust name so renaming a variant cannot silently invalidate rows
    /// already written to the database.
    pub fn as_str(self) -> &'static str {
        match self {
            EditKind::Substitution => "substitution",
            EditKind::Insertion => "insertion",
            EditKind::Deletion => "deletion",
            EditKind::CasingOnly => "casing",
            EditKind::PunctuationOnly => "punctuation",
            EditKind::NumberFormat => "number_format",
            EditKind::Rewrite => "rewrite",
        }
    }

    /// Inverse of [`EditKind::as_str`]. Unknown values (a row written by a newer
    /// build, then opened by an older one) fall back to `Rewrite`, the kind that
    /// is never auto-promoted — an unrecognised rule must not become active.
    pub fn from_stored(value: &str) -> EditKind {
        match value {
            "substitution" => EditKind::Substitution,
            "insertion" => EditKind::Insertion,
            "deletion" => EditKind::Deletion,
            "casing" => EditKind::CasingOnly,
            "punctuation" => EditKind::PunctuationOnly,
            "number_format" => EditKind::NumberFormat,
            _ => EditKind::Rewrite,
        }
    }
}

/// One localized change between the raw transcript and the edited one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// The original text, joined with single spaces. Empty for an insertion.
    pub before: String,
    /// The replacement text, joined with single spaces. Empty for a deletion.
    pub after: String,
    pub kind: EditKind,
}

/// Strip leading/trailing punctuation, returning the alphanumeric core.
///
/// Shared with the learning loop's `key_of` (see `managers::learning`), which
/// must normalise a correction exactly the way the correction *matcher* does -
/// otherwise "grandmaster" and "grandmaster," are one rule when applied and two
/// separate lessons when learned, and neither ever reaches the promotion
/// threshold.
pub(crate) fn core(word: &str) -> &str {
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

/// Word-level longest common subsequence, returned as index pairs.
///
/// Comparison is on the lowercased *core* of each word, so a pair differing only
/// in case or punctuation still aligns — which is exactly what lets those be
/// reported as `CasingOnly` / `PunctuationOnly` rather than as a substitution.
fn lcs_pairs(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let n = a.len();
    let m = b.len();
    let key = |w: &str| core(w).to_lowercase();
    let ka: Vec<String> = a.iter().map(|w| key(w)).collect();
    let kb: Vec<String> = b.iter().map(|w| key(w)).collect();

    // table[i][j] = LCS length of a[i..] and b[j..]
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if ka[i] == kb[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut pairs = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if ka[i] == kb[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Is `word` a spelled-out number or a digit string?
fn looks_numeric(word: &str) -> bool {
    const NUMBER_WORDS: &[&str] = &[
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
        "thirty",
        "forty",
        "fifty",
        "sixty",
        "seventy",
        "eighty",
        "ninety",
        "hundred",
        "thousand",
        "million",
        "billion",
        "percent",
        "dollars",
    ];
    let c = core(word).to_lowercase();
    if c.is_empty() {
        return false;
    }
    c.chars().any(|ch| ch.is_ascii_digit()) || NUMBER_WORDS.contains(&c.as_str())
}

/// Classify an aligned before/after span.
fn classify(before: &[&str], after: &[&str]) -> EditKind {
    if before.is_empty() {
        return EditKind::Insertion;
    }
    if after.is_empty() {
        return EditKind::Deletion;
    }

    let b_join = before.join(" ");
    let a_join = after.join(" ");

    // Same characters ignoring case → purely a capitalisation change.
    if b_join.to_lowercase() == a_join.to_lowercase() {
        return EditKind::CasingOnly;
    }

    // Same alphanumeric cores in the same order → only punctuation moved.
    let b_cores: Vec<String> = before.iter().map(|w| core(w).to_lowercase()).collect();
    let a_cores: Vec<String> = after.iter().map(|w| core(w).to_lowercase()).collect();
    if b_cores == a_cores {
        return EditKind::PunctuationOnly;
    }

    // Numbers on both sides, at least one side written differently.
    if before.iter().all(|w| looks_numeric(w)) && after.iter().all(|w| looks_numeric(w)) {
        return EditKind::NumberFormat;
    }

    // A span this large is a rephrase, not a mishearing. Four words is roughly
    // where "the model misheard a name" stops being a plausible explanation.
    if before.len() > 4 || after.len() > 4 {
        return EditKind::Rewrite;
    }

    EditKind::Substitution
}

/// Compute the typed edits between a raw transcript and its edited version.
///
/// Returns an empty vector when the texts are equal, when either is empty, or
/// when the input is too large to align (see [`MAX_WORDS`]).
pub fn diff_transcripts(original: &str, edited: &str) -> Vec<Edit> {
    if original == edited {
        return Vec::new();
    }

    let a: Vec<&str> = original.split_whitespace().collect();
    let b: Vec<&str> = edited.split_whitespace().collect();

    if a.is_empty() || b.is_empty() || a.len() > MAX_WORDS || b.len() > MAX_WORDS {
        return Vec::new();
    }

    let pairs = lcs_pairs(&a, &b);

    // Collect raw changed spans as index ranges first. Classification happens
    // after merging, because merging changes what a span *means*: "andy"→"Handy"
    // next to "plus"→"Plus" is one vocabulary correction, not a substitution
    // plus an unrelated instruction to capitalise the word "plus" everywhere.
    let mut spans: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;

    for (pi, pj) in pairs {
        if i != pi || j != pj {
            spans.push((i, pi, j, pj));
        }
        // An LCS anchor matched on the lowercased core, so the two words can
        // still differ in case or punctuation. That is a real change and must
        // become a span of its own — otherwise it would be silently dropped.
        if a[pi] != b[pj] {
            spans.push((pi, pi + 1, pj, pj + 1));
        }
        i = pi + 1;
        j = pj + 1;
    }
    if i != a.len() || j != b.len() {
        spans.push((i, a.len(), j, b.len()));
    }

    merge_adjacent(spans)
        .into_iter()
        .map(|(bs, be, as_, ae)| {
            let before = &a[bs..be];
            let after = &b[as_..ae];
            Edit {
                before: before.join(" "),
                after: after.join(" "),
                kind: classify(before, after),
            }
        })
        .collect()
}

/// Fuse spans that touch on both sides into a single span.
///
/// Two changes with no unchanged word between them are one correction the user
/// made in one motion, and splitting them produces rules that are wrong in
/// isolation. Editing "andy plus" to "Handy Plus" aligns "plus"/"Plus" as an
/// anchor, so a naive walk reports a substitution *and* a casing change — and
/// that casing change, promoted on its own, would capitalise the ordinary word
/// "plus" in every future transcript.
fn merge_adjacent(spans: Vec<(usize, usize, usize, usize)>) -> Vec<(usize, usize, usize, usize)> {
    let mut merged: Vec<(usize, usize, usize, usize)> = Vec::with_capacity(spans.len());

    for span in spans {
        match merged.last_mut() {
            // Contiguous in the original *and* the edited text.
            Some(last) if last.1 == span.0 && last.3 == span.2 => {
                last.1 = span.1;
                last.3 = span.3;
            }
            _ => merged.push(span),
        }
    }

    merged
}

/// Word-level edit distance, used by the divergence guard below.
fn word_edit_distance(a: &[String], b: &[String]) -> usize {
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];

    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// Default share of words an LLM cleanup pass may change before its output is
/// rejected. Legitimate cleanup removes fillers, fixes punctuation and converts
/// numbers; it does not rewrite half the sentence.
pub const DEFAULT_MAX_DIVERGENCE: f64 = 0.4;

/// Is `cleaned` a plausible cleanup of `original`, or has the model gone off and
/// written something else?
///
/// LLM post-processing is the one stage in the pipeline that can invent text.
/// A bad prompt, a confused model, or a prompt-injection inside the transcript
/// itself can all turn "tidy this up" into "answer this" or "summarise this",
/// and the result is pasted into whatever the user had focused. This is the
/// safety valve: a cleanup that diverges too far from what was actually said is
/// rejected in favour of the raw transcription.
///
/// Returns `true` when the output should be accepted.
pub fn is_plausible_cleanup(original: &str, cleaned: &str, max_divergence: f64) -> bool {
    let a: Vec<&str> = original.split_whitespace().collect();
    let b: Vec<&str> = cleaned.split_whitespace().collect();

    // Nothing was said; nothing to protect.
    if a.is_empty() {
        return true;
    }

    // Emptying a non-empty transcript is never a valid cleanup. The stock prompt
    // explicitly asks for "nothing at most a space" on empty input, so this only
    // fires when real speech was thrown away.
    if b.is_empty() {
        return false;
    }

    // Compare on lowercased, punctuation-stripped words. Fixing capitalisation
    // and punctuation is precisely what a cleanup pass is *for*, so charging
    // those against the divergence budget would reject the good case: tidying
    // "so um I was uh thinking" into "So I was thinking." touches nearly every
    // word by raw string comparison while changing almost nothing that matters.
    // What the budget must catch is the model substituting different *content*.
    let norm = |words: &[&str]| -> Vec<String> {
        words
            .iter()
            .map(|w| core(w).to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    };
    let na = norm(&a);
    let nb = norm(&b);

    // Everything on one side was pure punctuation. Treat as unchanged rather
    // than dividing by zero.
    if na.is_empty() {
        return true;
    }

    // The alignment below is O(n × m). On a transcript this long it would stall
    // the paste, so a bounded linear-time check stands in for it — see
    // [`is_plausible_long_cleanup`]. This used to `return true` outright, which
    // meant the one stage that can invent text was unguarded on exactly the
    // inputs where a runaway rewrite does the most damage.
    if a.len() > MAX_WORDS || b.len() > MAX_WORDS {
        return is_plausible_long_cleanup(&na, &nb, max_divergence);
    }

    let distance = word_edit_distance(&na, &nb) as f64;
    let denominator = na.len().max(nb.len()) as f64;
    (distance / denominator) <= max_divergence
}

/// Words per shingle in the long-transcript guard.
///
/// Three is the smallest width that still carries word order. Single words
/// would call a shuffled transcript unchanged; longer windows are broken by
/// every removed filler, which would penalise exactly the edit cleanup exists
/// to make.
const SHINGLE_WORDS: usize = 3;

/// Linear-time stand-in for [`is_plausible_cleanup`]'s alignment, for
/// transcripts too long to align in front of a waiting paste.
///
/// Two independent checks, both of which must pass:
///
/// 1. **Length.** Cleanup removes fillers and adds punctuation; it does not
///    change how much was said. A transcript that lost (or gained) more than
///    the divergence budget in sheer word count is a summary or an expansion,
///    not a tidy-up.
/// 2. **Survival.** Enough of the original's three-word sequences must still be
///    somewhere in the output. A model that went off and wrote its own prose
///    keeps almost none of them.
///
/// The survival threshold is derived from `max_divergence` rather than picked,
/// so the long path allows exactly what the exact path allows. If a fraction
/// `d` of scattered words may change, each surviving shingle needs all
/// [`SHINGLE_WORDS`] of its words to survive, so the expected survival rate is
/// `(1 - d)^SHINGLE_WORDS`. Choosing a rounder-looking number instead would
/// quietly make long transcripts stricter than short ones — punishing exactly
/// the disfluent, filler-heavy speech that cleanup helps most.
///
/// Both inputs are the already-normalised (lowercased, punctuation-stripped)
/// word lists, so casing and punctuation changes cost nothing here either.
fn is_plausible_long_cleanup(a: &[String], b: &[String], max_divergence: f64) -> bool {
    let longer = a.len().max(b.len());
    if longer == 0 {
        return true;
    }

    let length_change = a.len().abs_diff(b.len()) as f64 / longer as f64;
    if length_change > max_divergence {
        return false;
    }

    let before = shingles(a);
    if before.is_empty() {
        return true;
    }
    let after = shingles(b);

    let survived = before.iter().filter(|s| after.contains(*s)).count() as f64;
    let survival_ratio = survived / before.len() as f64;
    let required = (1.0 - max_divergence).powi(SHINGLE_WORDS as i32);

    survival_ratio >= required
}

/// Hashed [`SHINGLE_WORDS`]-word sequences of `words`.
///
/// Hashes rather than joined strings so a very long transcript costs a bounded
/// amount of memory. A collision can only make survival look slightly better
/// than it is, which errs toward accepting — the same direction as every other
/// fail-open decision in this pipeline.
fn shingles(words: &[String]) -> HashSet<u64> {
    let mut set = HashSet::new();
    if words.is_empty() {
        return set;
    }
    if words.len() < SHINGLE_WORDS {
        set.insert(hash_words(words));
        return set;
    }
    for window in words.windows(SHINGLE_WORDS) {
        set.insert(hash_words(window));
    }
    set
}

fn hash_words(words: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for word in words {
        word.hash(&mut hasher);
        // Separator, so ["ab", "c"] and ["a", "bc"] cannot collide.
        0xffu8.hash(&mut hasher);
    }
    hasher.finish()
}

/// Count how many times each `(before, after)` pair appears, keyed
/// case-insensitively so "Andy"/"andy" are the same lesson.
///
/// Frequency is the gate that keeps the learning loop from promoting a one-off
/// typo, or a moment where the user simply changed their mind, into a permanent
/// rule that will corrupt future transcripts.
pub fn tally_promotable(edits: &[Edit]) -> HashMap<(String, String), usize> {
    let mut counts = HashMap::new();
    for edit in edits.iter().filter(|e| e.kind.is_promotable()) {
        let key = (edit.before.to_lowercase(), edit.after.to_lowercase());
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(original: &str, edited: &str) -> Vec<EditKind> {
        diff_transcripts(original, edited)
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn identical_text_yields_no_edits() {
        assert!(diff_transcripts("same text", "same text").is_empty());
    }

    #[test]
    fn empty_inputs_yield_no_edits() {
        assert!(diff_transcripts("", "something").is_empty());
        assert!(diff_transcripts("something", "").is_empty());
    }

    #[test]
    fn single_word_substitution() {
        let edits = diff_transcripts("i use andy daily", "i use Handy daily");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].before, "andy");
        assert_eq!(edits[0].after, "Handy");
        assert_eq!(edits[0].kind, EditKind::Substitution);
    }

    #[test]
    fn multi_word_substitution_is_captured() {
        // The old single-word-only detector missed this entirely.
        let edits = diff_transcripts("i use andy plus daily", "i use Handy Plus daily");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].before, "andy plus");
        assert_eq!(edits[0].after, "Handy Plus");
        assert_eq!(edits[0].kind, EditKind::Substitution);
    }

    #[test]
    fn word_count_change_is_captured() {
        // The old detector required identical word counts and bailed here.
        let edits = diff_transcripts("use charge b daily", "use ChargeBee daily");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].before, "charge b");
        assert_eq!(edits[0].after, "ChargeBee");
    }

    #[test]
    fn insertion_is_classified() {
        assert_eq!(
            kinds("i use it", "i really use it"),
            vec![EditKind::Insertion]
        );
    }

    #[test]
    fn deletion_is_classified() {
        assert_eq!(
            kinds("i really use it", "i use it"),
            vec![EditKind::Deletion]
        );
    }

    #[test]
    fn casing_only_is_its_own_kind() {
        assert_eq!(
            kinds("i use iphone", "i use iPhone"),
            vec![EditKind::CasingOnly]
        );
    }

    #[test]
    fn punctuation_only_is_its_own_kind() {
        assert_eq!(
            kinds("hello there", "hello, there"),
            vec![EditKind::PunctuationOnly]
        );
    }

    #[test]
    fn number_formatting_is_its_own_kind() {
        assert_eq!(
            kinds("about twenty five", "about 25"),
            vec![EditKind::NumberFormat]
        );
    }

    #[test]
    fn a_large_span_change_is_a_rewrite_not_a_substitution() {
        let edits = diff_transcripts(
            "the quick brown fox jumps over the lazy dog",
            "a completely different sentence entirely written here instead now",
        );
        assert!(
            edits.iter().any(|e| e.kind == EditKind::Rewrite),
            "expected a Rewrite, got {edits:?}"
        );
    }

    #[test]
    fn multiple_independent_edits_are_all_found() {
        let edits = diff_transcripts("andy is on ios today", "Handy is on iOS today");
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].after, "Handy");
        assert_eq!(edits[1].after, "iOS");
    }

    // ── promotion policy ─────────────────────────────────────────────────────

    #[test]
    fn only_safe_kinds_are_promotable() {
        assert!(EditKind::Substitution.is_promotable());
        assert!(EditKind::CasingOnly.is_promotable());
        assert!(EditKind::NumberFormat.is_promotable());

        // These must never become automatic rules.
        assert!(!EditKind::Rewrite.is_promotable());
        assert!(!EditKind::Insertion.is_promotable());
        assert!(!EditKind::Deletion.is_promotable());
        assert!(!EditKind::PunctuationOnly.is_promotable());
    }

    #[test]
    fn only_substitutions_teach_vocabulary() {
        assert!(EditKind::Substitution.is_vocabulary());
        assert!(!EditKind::CasingOnly.is_vocabulary());
        assert!(!EditKind::Rewrite.is_vocabulary());
    }

    #[test]
    fn tally_counts_repeats_case_insensitively() {
        let edits = vec![
            Edit {
                before: "andy".into(),
                after: "Handy".into(),
                kind: EditKind::Substitution,
            },
            Edit {
                before: "Andy".into(),
                after: "handy".into(),
                kind: EditKind::Substitution,
            },
            Edit {
                before: "x".into(),
                after: "y".into(),
                kind: EditKind::Rewrite,
            },
        ];
        let counts = tally_promotable(&edits);
        assert_eq!(counts.get(&("andy".into(), "handy".into())), Some(&2));
        // The rewrite is excluded from the tally entirely.
        assert_eq!(counts.len(), 1);
    }

    // ── divergence guard ─────────────────────────────────────────────────────

    #[test]
    fn identical_cleanup_is_plausible() {
        assert!(is_plausible_cleanup("hello world", "hello world", 0.4));
    }

    #[test]
    fn removing_filler_words_is_plausible() {
        assert!(is_plausible_cleanup(
            "so um I was uh thinking about this",
            "So I was thinking about this.",
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn answering_the_question_instead_of_cleaning_it_is_rejected() {
        // The classic post-processing failure: the model obeys the transcript
        // instead of tidying it.
        assert!(!is_plausible_cleanup(
            "hey what is the capital of France",
            "The capital of France is Paris. It has been the capital since 508 AD and is known for the Eiffel Tower.",
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn emptying_a_real_transcript_is_rejected() {
        assert!(!is_plausible_cleanup("this was real speech", "", 0.4));
        assert!(!is_plausible_cleanup("this was real speech", "   ", 0.4));
    }

    #[test]
    fn empty_original_is_always_accepted() {
        assert!(is_plausible_cleanup("", "anything", 0.4));
        assert!(is_plausible_cleanup("   ", "", 0.4));
    }

    #[test]
    fn a_total_rewrite_is_rejected() {
        assert!(!is_plausible_cleanup(
            "one two three four five",
            "alpha beta gamma delta epsilon",
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn a_summary_of_a_long_transcript_is_rejected() {
        let original = "so I went to the shop and I bought some milk and then I walked home again";
        assert!(!is_plausible_cleanup(
            original,
            "User bought milk.",
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn threshold_is_respected_at_both_extremes() {
        // 0.0 accepts only an exact match; 1.0 accepts anything.
        assert!(is_plausible_cleanup("a b c", "a b c", 0.0));
        assert!(!is_plausible_cleanup("a b c", "a b d", 0.0));
        assert!(is_plausible_cleanup("a b c", "x y z", 1.0));
    }

    /// A transcript too long to align is now checked by the bounded guard
    /// instead of being waved through. This previously asserted the opposite —
    /// that the same input was *accepted* — which is the hole being closed.
    #[test]
    fn a_very_long_transcript_replaced_by_a_summary_is_rejected() {
        let long = "word ".repeat(MAX_WORDS + 10);
        assert!(!is_plausible_cleanup(
            &long,
            "short",
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn a_very_long_transcript_still_accepts_ordinary_cleanup() {
        // Ten words per sentence, one of them a filler the cleanup removes,
        // plus casing and punctuation the guard is supposed to ignore.
        let original = "um so the plan for today is simple and clear ".repeat(900);
        let cleaned = "So the plan for today is simple and clear. ".repeat(900);
        assert!(original.split_whitespace().count() > MAX_WORDS);
        assert!(is_plausible_cleanup(
            &original,
            &cleaned,
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn a_very_long_transcript_rewritten_wholesale_is_rejected() {
        let original = "the quick brown fox jumps over the lazy dog ".repeat(300);
        let cleaned =
            "completely different prose about unrelated matters entirely written now ".repeat(300);
        assert!(original.split_whitespace().count() > MAX_WORDS);
        // Same length, so only the survival check can catch this.
        assert!(!is_plausible_cleanup(
            &original,
            &cleaned,
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    /// Filler-heavy speech is precisely what cleanup is for, so the long path
    /// must not be stricter than the exact path about it. One word in five is
    /// removed here, which is well beyond a realistic filler rate.
    #[test]
    fn a_very_long_disfluent_transcript_is_still_cleaned_up() {
        let original = "um so the plan is uh simple and clear ".repeat(900);
        let cleaned = "So the plan is simple and clear. ".repeat(900);
        assert!(original.split_whitespace().count() > MAX_WORDS);
        assert!(is_plausible_cleanup(
            &original,
            &cleaned,
            DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn a_very_long_transcript_is_accepted_when_untouched() {
        let long = "one two three four five six seven eight nine ten ".repeat(400);
        assert!(is_plausible_cleanup(&long, &long, DEFAULT_MAX_DIVERGENCE));
    }

    #[test]
    fn oversized_diff_input_is_skipped_safely() {
        let long = "word ".repeat(MAX_WORDS + 10);
        assert!(diff_transcripts(&long, "short").is_empty());
    }
}
