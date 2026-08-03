//! Deterministic application of user-taught correction pairs.
//!
//! A `CorrectionPair { wrong, correct }` is something the user explicitly taught
//! Handy Plus after fixing a transcript by hand ("andy plus" → "Handy Plus").
//! Before this module existed the `wrong` side was never used for anything
//! except being listed in the LLM post-processing prompt, so teaching a pair had
//! no effect at all unless a cloud/Apple LLM pass was enabled. This applies the
//! pairs directly, on-device, with no model involved.
//!
//! ## Design rules
//!
//! These are deliberate and each one closes a specific failure mode:
//!
//! * **Whole-token matching.** Matching is done against the alphanumeric *core*
//!   of a whitespace-delimited token, so a rule for `andy` can never fire inside
//!   `candy`. Substring replacement is the classic way a find-and-replace feature
//!   silently corrupts correct text; it is structurally impossible here.
//! * **No rescanning.** After a replacement the scan resumes *after* the replaced
//!   span. Replacement output is never re-examined, so rule cycles
//!   (`a → b`, `b → a`) cannot loop and rules cannot cascade into each other.
//!   This makes the pass trivially idempotent and O(tokens × rules).
//! * **Leftmost-longest, no reconsideration.** At each starting token the longest
//!   matching rule wins; the scan then resumes past the consumed span. A rule
//!   whose phrase *starts inside* an already-consumed span is therefore not
//!   considered at all — given rules `andy plus` and `plus one`, the text
//!   "andy plus one" resolves the first and never evaluates the second. This is
//!   a deliberate choice, not an accident: the alternative (rescanning to find
//!   overlapping alternatives) reintroduces cascade and cycle risk for a case
//!   that is rare and ambiguous anyway. Documented so the behaviour is
//!   explainable when someone asks why their second rule "didn't fire".
//! * **Line breaks are hard boundaries.** A multi-word rule will not match across
//!   a newline. The replacement is a single canonical string, so matching across
//!   one would silently swallow a line or paragraph break — the same class of
//!   damage as the whitespace-collapsing bug this module avoids.
//! * **Whitespace is preserved byte-for-byte.** Everything outside a replaced
//!   span is copied verbatim, so newlines and paragraph breaks survive.
//! * **No interior punctuation.** A multi-word rule will not match across a
//!   comma or full stop, so "Charge, B" is not collapsed into one term. This
//!   mirrors the existing fuzzy matcher's boundary rule in [`super::text`].
//! * **Case-insensitive matching, case-pattern-preserving output.** See
//!   [`apply_correction_pairs`].

/// Hard ceiling on rules considered in one pass. Matching is O(tokens × rules);
/// this bounds worst-case latency for a dictionary that has grown for years.
/// Rules beyond the cap are ignored rather than slowing every transcription.
const MAX_RULES: usize = 512;

/// Hard ceiling on words in a single rule's `wrong` side. Guards against a
/// pathological paste into the settings UI turning into a long scan window.
const MAX_RULE_WORDS: usize = 8;

/// One compiled rule: the lowercased word-cores of the `wrong` side, plus the
/// replacement to emit.
struct Rule<'a> {
    words: Vec<String>,
    correct: &'a str,
}

/// A whitespace-delimited token, located by byte offsets into the source.
struct Token {
    /// Byte range of the whole token, punctuation included.
    start: usize,
    end: usize,
    /// Byte range of the alphanumeric core. Equal to `start..start` when the
    /// token contains no alphanumeric character at all (e.g. a bare "—").
    core_start: usize,
    core_end: usize,
}

impl Token {
    fn core<'a>(&self, text: &'a str) -> &'a str {
        &text[self.core_start..self.core_end]
    }

    /// Trailing punctuation, e.g. the "," of "B,".
    fn has_suffix(&self) -> bool {
        self.core_end < self.end
    }
}

/// Locate the alphanumeric core of a token, as byte offsets relative to `tok`.
///
/// Uses `char_indices` on both ends so multibyte punctuation (`。`, `「」`) can
/// never be split mid-character.
fn core_range(tok: &str) -> (usize, usize) {
    let start = tok
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, _)| i);
    match start {
        None => (0, 0),
        Some(s) => {
            let e = tok
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_alphanumeric())
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(s);
            (s, e)
        }
    }
}

/// Split `text` into whitespace-delimited tokens with absolute byte offsets.
fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;

    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                tokens.push(make_token(text, s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        tokens.push(make_token(text, s, text.len()));
    }
    tokens
}

fn make_token(text: &str, start: usize, end: usize) -> Token {
    let (cs, ce) = core_range(&text[start..end]);
    Token {
        start,
        end,
        core_start: start + cs,
        core_end: start + ce,
    }
}

/// Compile `(wrong, correct)` pairs into match rules, longest phrase first.
///
/// Pairs are dropped when either side is blank, when the `wrong` side has no
/// alphanumeric content to match on, or when it exceeds [`MAX_RULE_WORDS`].
/// A blank `correct` is rejected specifically so a half-filled settings row can
/// never turn into a delete-this-text rule.
fn compile<'a, I>(pairs: I) -> Vec<Rule<'a>>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut rules: Vec<Rule<'a>> = Vec::new();

    for (wrong, correct) in pairs {
        if rules.len() >= MAX_RULES {
            break;
        }
        if correct.trim().is_empty() {
            continue;
        }

        let words: Vec<String> = wrong
            .split_whitespace()
            .map(|w| {
                let (s, e) = core_range(w);
                w[s..e].to_lowercase()
            })
            .collect();

        if words.is_empty() || words.len() > MAX_RULE_WORDS || words.iter().any(|w| w.is_empty()) {
            continue;
        }

        rules.push(Rule { words, correct });
    }

    // Longest phrase first so "handy plus" is tried before "handy". `sort_by` is
    // stable, so equal-length rules keep the user's ordering and the first-added
    // duplicate wins.
    rules.sort_by(|a, b| b.words.len().cmp(&a.words.len()));
    rules
}

/// Does `rule` match the token run starting at `i`?
fn matches_at(text: &str, tokens: &[Token], i: usize, rule: &Rule) -> bool {
    let n = rule.words.len();
    if i + n > tokens.len() {
        return false;
    }
    for (k, want) in rule.words.iter().enumerate() {
        let tok = &tokens[i + k];
        if k + 1 < n {
            // Do not consume across a punctuation boundary: in "Charge B, che"
            // the comma closes the candidate at "B,". Only interior tokens are
            // checked — trailing punctuation on the *final* token is preserved,
            // not matched against.
            if tok.has_suffix() {
                return false;
            }
            // Nor across a line break. The replacement collapses the matched
            // span into one string, so matching across a newline would delete
            // it. Plain spaces and tabs are fine to absorb; line structure is
            // not ours to discard.
            let gap = &text[tok.end..tokens[i + k + 1].start];
            if gap.contains('\n') || gap.contains('\r') {
                return false;
            }
        }
        if !tok.core(text).eq_ignore_ascii_case(want) && tok.core(text).to_lowercase() != *want {
            return false;
        }
    }
    true
}

/// Reshape `replacement` to follow the case pattern of the text it replaces.
///
/// * ALL CAPS original → uppercase replacement ("ANDY PLUS" → "HANDY PLUS")
/// * Capitalized original → capitalize the replacement's first character
/// * anything else → the taught spelling verbatim, so "ios" → "iOS" keeps its
///   intended casing rather than being flattened
fn apply_case_pattern(original_core: &str, replacement: &str) -> String {
    let has_alpha = original_core.chars().any(|c| c.is_alphabetic());

    if has_alpha
        && original_core
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(|c| c.is_uppercase())
    {
        return replacement.to_uppercase();
    }

    if original_core
        .chars()
        .next()
        .is_some_and(|c| c.is_uppercase())
    {
        let mut chars = replacement.chars();
        return match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
    }

    replacement.to_string()
}

/// The result of a correction pass: the rewritten text, plus where the taught
/// replacements landed in it.
pub struct CorrectionOutcome {
    pub text: String,
    /// Byte ranges **within `text`** occupied by a user-taught replacement.
    ///
    /// These exist so a later, *probabilistic* pass can be told to keep its
    /// hands off. The fuzzy custom-word matcher works over 1–3-word n-grams; a
    /// deterministic replacement creates word adjacencies that never existed in
    /// the raw transcription ("Handy Plus daily"), and the fuzzy pass would
    /// happily score those new n-grams against its dictionary. Since the user
    /// stated this exact spelling by hand, nothing downstream gets a vote.
    pub protected: Vec<std::ops::Range<usize>>,
}

/// Apply user-taught `(wrong, correct)` corrections to `text`, reporting where
/// the replacements landed.
///
/// Matching is case-insensitive — a user who teaches "andy plus → Handy Plus"
/// expects it to fire on "Andy Plus" too. Speech-to-text capitalisation is the
/// model's guess at sentence position, not something the speaker uttered, so
/// treating it as a match signal would make a taught pair fire mid-sentence and
/// silently miss at the start of the next one. The replacement then follows the
/// *original's* case pattern (see [`apply_case_pattern`]), so shouting is
/// preserved and the taught spelling wins everywhere else.
///
/// Returns `text` unchanged, with no protected ranges, when there are no usable
/// rules — so callers with an empty dictionary pay nothing and behave exactly as
/// they did before this pass existed.
pub fn apply_correction_pairs_tracked<'a, I>(text: &str, pairs: I) -> CorrectionOutcome
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let unchanged = || CorrectionOutcome {
        text: text.to_string(),
        protected: Vec::new(),
    };

    let rules = compile(pairs);
    if rules.is_empty() {
        return unchanged();
    }

    let tokens = tokenize(text);
    if tokens.is_empty() {
        return unchanged();
    }

    let mut out = String::with_capacity(text.len());
    let mut protected = Vec::new();
    let mut cursor = 0usize; // byte offset of the first not-yet-emitted char
    let mut i = 0usize; // token index

    while i < tokens.len() {
        let hit = rules.iter().find(|rule| matches_at(text, &tokens, i, rule));

        match hit {
            Some(rule) => {
                let n = rule.words.len();
                let first = &tokens[i];
                let last = &tokens[i + n - 1];

                // Everything before this token (whitespace, earlier text).
                out.push_str(&text[cursor..first.start]);
                // Leading punctuation of the first token, e.g. the "(" of "(andy".
                out.push_str(&text[first.start..first.core_start]);

                let replaced_from = out.len();
                out.push_str(&apply_case_pattern(first.core(text), rule.correct));
                protected.push(replaced_from..out.len());

                // Trailing punctuation of the last token, e.g. the "," of "plus,".
                out.push_str(&text[last.core_end..last.end]);

                cursor = last.end;
                // Resume *after* the replaced span — replacement output is never
                // rescanned, which is what makes rule cycles impossible.
                i += n;
            }
            None => i += 1,
        }
    }

    out.push_str(&text[cursor..]);
    CorrectionOutcome {
        text: out,
        protected,
    }
}

/// [`apply_correction_pairs_tracked`] when the caller does not need the ranges.
pub fn apply_correction_pairs<'a, I>(text: &str, pairs: I) -> String
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    apply_correction_pairs_tracked(text, pairs).text
}

/// Split `text` into `(segment, is_protected)` runs using ranges produced by
/// [`apply_correction_pairs_tracked`].
///
/// Lets a caller run a probabilistic pass over only the unprotected runs while
/// copying taught replacements through untouched. With no protected ranges this
/// yields exactly one unprotected segment covering the whole string, so the
/// caller's behaviour is bit-identical to not using it at all.
pub fn split_protected<'a>(
    text: &'a str,
    protected: &[std::ops::Range<usize>],
) -> Vec<(&'a str, bool)> {
    if protected.is_empty() {
        return vec![(text, false)];
    }

    let mut out = Vec::with_capacity(protected.len() * 2 + 1);
    let mut cursor = 0usize;

    for range in protected {
        // Defensive: ignore anything that is not a well-formed, in-bounds,
        // forward-ordered range rather than panicking on a slice.
        if range.start < cursor || range.end > text.len() || range.start >= range.end {
            continue;
        }
        if !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
            continue;
        }
        if range.start > cursor {
            out.push((&text[cursor..range.start], false));
        }
        out.push((&text[range.start..range.end], true));
        cursor = range.end;
    }

    if cursor < text.len() {
        out.push((&text[cursor..], false));
    }
    out
}

/// Run `f` over every unprotected run of `text`, copying taught replacements
/// through untouched, and reassemble the result.
///
/// The whitespace bookkeeping here is load-bearing. The fuzzy custom-word pass
/// this is designed to wrap normalises its input with `split_whitespace()` +
/// `join(" ")`, which discards leading and trailing whitespace. Handing it a
/// segment like `"use "` would return `"use"`, and naively concatenating the
/// pieces would produce `"useHandy Plus"` — words fused together. So each
/// unprotected run is split into leading whitespace / core / trailing
/// whitespace, and only the core is passed to `f`.
///
/// With no protected ranges this is `f` applied to the whole string, modulo
/// outer whitespace that the caller's own trimming removes anyway — so callers
/// with no correction pairs are unaffected.
pub fn apply_outside_protected<F>(
    text: &str,
    protected: &[std::ops::Range<usize>],
    mut f: F,
) -> String
where
    F: FnMut(&str) -> String,
{
    let mut out = String::with_capacity(text.len());

    for (segment, is_protected) in split_protected(text, protected) {
        if is_protected {
            out.push_str(segment);
            continue;
        }

        let core = segment.trim();
        if core.is_empty() {
            // Whitespace-only gap between two replacements: keep it verbatim.
            out.push_str(segment);
            continue;
        }

        let core_start = segment.len() - segment.trim_start().len();
        let core_end = core_start + core.len();

        out.push_str(&segment[..core_start]);
        out.push_str(&f(core));
        out.push_str(&segment[core_end..]);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, pairs: &[(&str, &str)]) -> String {
        apply_correction_pairs(text, pairs.iter().map(|(w, c)| (*w, *c)))
    }

    #[test]
    fn applies_a_simple_pair() {
        assert_eq!(
            apply("i use andy every day", &[("andy", "Handy")]),
            "i use Handy every day"
        );
    }

    #[test]
    fn never_matches_inside_a_longer_word() {
        // The whole point of core-token matching: "andy" must not touch "candy".
        assert_eq!(
            apply("i like candy and andyman", &[("andy", "Handy")]),
            "i like candy and andyman"
        );
    }

    #[test]
    fn matches_case_insensitively() {
        assert_eq!(apply("Andy is here", &[("andy", "Handy")]), "Handy is here");
    }

    #[test]
    fn preserves_all_caps() {
        assert_eq!(apply("ANDY rules", &[("andy", "Handy")]), "HANDY rules");
    }

    #[test]
    fn lowercase_original_keeps_the_taught_spelling() {
        // "ios" must become "iOS", not "Ios".
        assert_eq!(apply("on ios today", &[("ios", "iOS")]), "on iOS today");
    }

    #[test]
    fn capitalized_original_capitalizes_replacement() {
        assert_eq!(
            apply("Andy plus rocks", &[("andy plus", "handy plus")]),
            "Handy plus rocks"
        );
    }

    #[test]
    fn multi_word_phrase() {
        assert_eq!(
            apply("i use andy plus daily", &[("andy plus", "Handy Plus")]),
            "i use Handy Plus daily"
        );
    }

    #[test]
    fn longest_phrase_wins() {
        // The single-word rule is listed first but must not pre-empt the phrase.
        assert_eq!(
            apply(
                "andy plus is great",
                &[("andy", "Handy"), ("andy plus", "Handy Plus")]
            ),
            "Handy Plus is great"
        );
    }

    #[test]
    fn preserves_surrounding_punctuation() {
        assert_eq!(
            apply("(andy), really?", &[("andy", "Handy")]),
            "(Handy), really?"
        );
    }

    #[test]
    fn does_not_match_across_interior_punctuation() {
        // "Charge, B" is two clauses, not the product name.
        assert_eq!(
            apply("charge, b is fine", &[("charge b", "ChargeBee")]),
            "charge, b is fine"
        );
    }

    #[test]
    fn keeps_trailing_punctuation_on_a_phrase_match() {
        assert_eq!(
            apply("i use charge b, daily", &[("charge b", "ChargeBee")]),
            "i use ChargeBee, daily"
        );
    }

    #[test]
    fn preserves_newlines_and_spacing() {
        // The existing fuzzy pass collapses all whitespace via
        // split_whitespace() + join(" "); this pass must not repeat that.
        let text = "first andy\n\nsecond  andy\tthird";
        assert_eq!(
            apply(text, &[("andy", "Handy")]),
            "first Handy\n\nsecond  Handy\tthird"
        );
    }

    #[test]
    fn is_idempotent() {
        let pairs = [("andy plus", "Handy Plus")];
        let once = apply("andy plus", &pairs);
        let twice = apply(&once, &pairs);
        assert_eq!(once, twice);
        assert_eq!(once, "Handy Plus");
    }

    #[test]
    fn cyclic_rules_cannot_loop() {
        // a -> b and b -> a. Without the no-rescan rule this would spin or
        // oscillate; here each token is decided exactly once.
        let out = apply("alpha beta", &[("alpha", "beta"), ("beta", "alpha")]);
        assert_eq!(out, "beta alpha");
    }

    #[test]
    fn replacement_containing_the_trigger_does_not_recurse() {
        assert_eq!(apply("handy", &[("handy", "handy plus")]), "handy plus");
    }

    #[test]
    fn skips_blank_sides() {
        assert_eq!(
            apply("keep this", &[("", "X"), ("this", "  ")]),
            "keep this"
        );
    }

    #[test]
    fn skips_rules_whose_wrong_side_has_no_alphanumerics() {
        assert_eq!(apply("a -- b", &[("--", "—")]), "a -- b");
    }

    #[test]
    fn empty_input_and_empty_rules() {
        assert_eq!(apply("", &[("a", "b")]), "");
        assert_eq!(apply("unchanged", &[]), "unchanged");
    }

    #[test]
    fn whitespace_only_input_is_returned_verbatim() {
        assert_eq!(apply("   \n  ", &[("a", "b")]), "   \n  ");
    }

    #[test]
    fn handles_unicode_text_and_punctuation() {
        assert_eq!(apply("「handee。」", &[("handee", "Handy")]), "「Handy。」");
    }

    #[test]
    fn handles_non_ascii_rules() {
        assert_eq!(apply("cafe au lait", &[("cafe", "café")]), "café au lait");
        assert_eq!(apply("Café time", &[("café", "coffee")]), "Coffee time");
    }

    #[test]
    fn rule_longer_than_remaining_tokens_does_not_panic() {
        assert_eq!(apply("andy", &[("andy plus pro max", "X")]), "andy");
    }

    #[test]
    fn overlong_rules_are_ignored() {
        let long = "a b c d e f g h i j";
        assert_eq!(apply("a b c d e f g h i j", &[(long, "X")]), long);
    }

    #[test]
    fn adjacent_matches_both_fire() {
        assert_eq!(
            apply("andy andy andy", &[("andy", "Handy")]),
            "Handy Handy Handy"
        );
    }

    #[test]
    fn first_duplicate_rule_wins() {
        assert_eq!(apply("x", &[("x", "first"), ("x", "second")]), "first");
    }

    #[test]
    fn leading_and_trailing_whitespace_survives() {
        assert_eq!(apply("  andy  ", &[("andy", "Handy")]), "  Handy  ");
    }

    #[test]
    fn numeric_and_mixed_cores_match() {
        assert_eq!(apply("gpt4 rocks", &[("gpt4", "GPT-4")]), "GPT-4 rocks");
    }

    // ── line breaks are hard boundaries ──────────────────────────────────────

    #[test]
    fn phrase_does_not_match_across_a_newline() {
        // Collapsing this span would eat the line break. Leave it alone.
        assert_eq!(
            apply("andy\nplus is here", &[("andy plus", "Handy Plus")]),
            "andy\nplus is here"
        );
    }

    #[test]
    fn phrase_does_not_match_across_a_paragraph_break() {
        assert_eq!(
            apply("andy\n\nplus", &[("andy plus", "Handy Plus")]),
            "andy\n\nplus"
        );
    }

    #[test]
    fn phrase_still_matches_across_spaces_and_tabs() {
        // Intra-line whitespace is absorbed into the replacement by design;
        // only line structure is protected.
        assert_eq!(
            apply("andy  \tplus ok", &[("andy plus", "Handy Plus")]),
            "Handy Plus ok"
        );
    }

    #[test]
    fn single_word_rule_is_unaffected_by_newlines() {
        assert_eq!(apply("andy\nandy", &[("andy", "Handy")]), "Handy\nHandy");
    }

    // ── documented leftmost-longest behaviour ────────────────────────────────

    #[test]
    fn overlapping_rules_resolve_leftmost_longest_without_reconsideration() {
        // Both rules are legitimate; "andy plus" starts first and wins, so
        // "plus one" is never evaluated. Asserted so the behaviour is a decision
        // rather than an accident.
        assert_eq!(
            apply(
                "andy plus one",
                &[("andy plus", "Handy Plus"), ("plus one", "PlusOne")]
            ),
            "Handy Plus one"
        );
    }

    // ── protected ranges / seam with the fuzzy pass ──────────────────────────

    #[test]
    fn tracks_the_range_of_each_replacement() {
        let out =
            apply_correction_pairs_tracked("use andy plus daily", [("andy plus", "Handy Plus")]);
        assert_eq!(out.text, "use Handy Plus daily");
        assert_eq!(out.protected.len(), 1);
        let r = out.protected[0].clone();
        assert_eq!(&out.text[r], "Handy Plus");
    }

    #[test]
    fn tracks_multiple_replacements_in_order() {
        let out =
            apply_correction_pairs_tracked("andy and ios", [("andy", "Handy"), ("ios", "iOS")]);
        assert_eq!(out.text, "Handy and iOS");
        let spans: Vec<&str> = out.protected.iter().map(|r| &out.text[r.clone()]).collect();
        assert_eq!(spans, vec!["Handy", "iOS"]);
    }

    #[test]
    fn no_rules_means_no_protected_ranges() {
        let out = apply_correction_pairs_tracked("plain text", std::iter::empty());
        assert_eq!(out.text, "plain text");
        assert!(out.protected.is_empty());
    }

    #[test]
    fn split_protected_round_trips_the_whole_string() {
        let out =
            apply_correction_pairs_tracked("use andy plus daily", [("andy plus", "Handy Plus")]);
        let parts = split_protected(&out.text, &out.protected);
        let rejoined: String = parts.iter().map(|(s, _)| *s).collect();
        assert_eq!(rejoined, out.text);
        assert_eq!(
            parts,
            vec![("use ", false), ("Handy Plus", true), (" daily", false)]
        );
    }

    #[test]
    fn split_protected_with_no_ranges_is_one_unprotected_segment() {
        // Guarantees byte-identical behaviour for users with no correction pairs.
        assert_eq!(
            split_protected("hello world", &[]),
            vec![("hello world", false)]
        );
    }

    #[test]
    fn split_protected_handles_a_leading_and_trailing_span() {
        let out = apply_correction_pairs_tracked("andy", [("andy", "Handy")]);
        assert_eq!(
            split_protected(&out.text, &out.protected),
            vec![("Handy", true)]
        );
    }

    // ── apply_outside_protected ──────────────────────────────────────────────

    #[test]
    fn apply_outside_protected_does_not_fuse_words() {
        // The regression this function exists to prevent: a wrapped pass that
        // trims its input must not cause "use " + "Handy Plus" to become
        // "useHandy Plus".
        let out =
            apply_correction_pairs_tracked("use andy plus daily", [("andy plus", "Handy Plus")]);
        // Simulate the fuzzy pass's normalisation: split_whitespace + join.
        let trimming = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let result = apply_outside_protected(&out.text, &out.protected, trimming);
        assert_eq!(result, "use Handy Plus daily");
    }

    #[test]
    fn apply_outside_protected_never_passes_protected_text_to_f() {
        let out = apply_correction_pairs_tracked("andy", [("andy", "Handy")]);
        let mut seen: Vec<String> = Vec::new();
        let result = apply_outside_protected(&out.text, &out.protected, |s| {
            seen.push(s.to_string());
            "MANGLED".to_string()
        });
        assert_eq!(result, "Handy");
        assert!(seen.is_empty(), "f was called on protected text: {seen:?}");
    }

    #[test]
    fn apply_outside_protected_with_no_ranges_applies_f_to_the_core() {
        let result = apply_outside_protected("hello world", &[], |s| s.to_uppercase());
        assert_eq!(result, "HELLO WORLD");
    }

    #[test]
    fn apply_outside_protected_keeps_whitespace_between_adjacent_replacements() {
        let out = apply_correction_pairs_tracked("andy ios", [("andy", "Handy"), ("ios", "iOS")]);
        let result = apply_outside_protected(&out.text, &out.protected, |s| s.to_string());
        assert_eq!(result, "Handy iOS");
    }

    #[test]
    fn apply_outside_protected_preserves_newlines_around_a_replacement() {
        let out = apply_correction_pairs_tracked("line one\nandy\nline three", [("andy", "Handy")]);
        let result = apply_outside_protected(&out.text, &out.protected, |s| s.to_string());
        assert_eq!(result, "line one\nHandy\nline three");
    }

    #[test]
    fn split_protected_ignores_malformed_ranges() {
        // Out of bounds, inverted, and non-char-boundary ranges are skipped
        // rather than panicking on a slice. Ranges are built with explicit
        // struct literals because `3..1` is a compile-time clippy error
        // (`reversed_empty_ranges`) — which is precisely the malformed input
        // this test needs to feed in.
        let text = "héllo";
        let malformed = [
            std::ops::Range { start: 0, end: 999 },
            std::ops::Range { start: 3, end: 1 },
            // 2 lands inside the two-byte 'é'.
            std::ops::Range { start: 2, end: 3 },
        ];

        for range in malformed {
            assert_eq!(
                split_protected(text, std::slice::from_ref(&range)),
                vec![(text, false)],
                "malformed range {range:?} should be ignored"
            );
        }
    }

    /// End-to-end tests over the composed text pipeline, mirroring
    /// `post_process_transcription_text` in `managers::transcription`.
    ///
    /// These exercise the *seam* between this deterministic pass and the upstream
    /// fuzzy matcher in [`super::super::text`] — the interaction is where the real
    /// risk lives, and neither module's own unit tests can see it. Two of these
    /// tests exist specifically to pin backward compatibility: with no correction
    /// pairs configured, output must be byte-identical to the pipeline as it
    /// behaved before this module existed.
    ///
    /// Nested inside `tests` rather than declared as a sibling module so nothing
    /// follows the test module at file scope (clippy::items_after_test_module).
    mod pipeline_tests {
        use super::*;
        use crate::audio_toolkit::text::{apply_custom_words, filter_transcription_output};

        /// Mirrors `post_process_transcription_text`.
        fn pipeline(
            raw: &str,
            pairs: &[(&str, &str)],
            custom_words: &[&str],
            already_prompted: bool,
        ) -> String {
            let p = apply_correction_pairs_tracked(raw, pairs.iter().map(|(w, c)| (*w, *c)));

            // Mirrors `AppSettings::effective_custom_words()`.
            let mut effective: Vec<String> = custom_words.iter().map(|s| s.to_string()).collect();
            effective.extend(pairs.iter().map(|(_, c)| c.to_string()));

            let corrected = if !effective.is_empty() && !already_prompted {
                apply_outside_protected(&p.text, &p.protected, |segment| {
                    apply_custom_words(segment, &effective, 0.18)
                })
            } else {
                p.text
            };

            filter_transcription_output(&corrected, "en", &None)
        }

        #[test]
        fn a_taught_pair_now_actually_applies() {
            // The headline fix. Before this module the `wrong` side of a correction
            // pair was never used by anything except the LLM prompt string.
            assert_eq!(
                pipeline(
                    "i use andy plus daily",
                    &[("andy plus", "Handy Plus")],
                    &[],
                    false
                ),
                "i use Handy Plus daily"
            );
        }

        #[test]
        fn a_taught_pair_applies_on_whisper_where_the_fuzzy_pass_is_skipped() {
            assert_eq!(
                pipeline(
                    "i use andy plus daily",
                    &[("andy plus", "Handy Plus")],
                    &[],
                    true
                ),
                "i use Handy Plus daily"
            );
        }

        #[test]
        fn the_seam_does_not_fuse_words_together() {
            let out = pipeline(
                "use andy plus daily",
                &[("andy plus", "Handy Plus")],
                &[],
                false,
            );
            assert!(!out.contains("useHandy"), "words were fused: {out}");
            assert_eq!(out, "use Handy Plus daily");
        }

        #[test]
        fn the_fuzzy_pass_cannot_rewrite_a_taught_span() {
            let out = pipeline(
                "andy plus report",
                &[("andy plus", "Handy Plus")],
                &["HandyPlusReport"],
                false,
            );
            assert!(
                out.starts_with("Handy Plus"),
                "a protected span was rewritten: {out}"
            );
        }

        #[test]
        fn the_fuzzy_pass_still_fires_outside_protected_spans() {
            let out = pipeline(
                "andy plus and chargebe",
                &[("andy plus", "Handy Plus")],
                &["ChargeBee"],
                false,
            );
            assert!(out.contains("Handy Plus"), "{out}");
            assert!(
                out.contains("ChargeBee"),
                "fuzzy matching stopped working outside the span: {out}"
            );
        }

        #[test]
        fn output_is_unchanged_for_users_with_custom_words_but_no_pairs() {
            let raw = "helo wrold and chargebe";
            let words = ["hello", "world", "ChargeBee"];
            let effective: Vec<String> = words.iter().map(|s| s.to_string()).collect();

            let before = filter_transcription_output(
                &apply_custom_words(raw, &effective, 0.18),
                "en",
                &None,
            );
            assert_eq!(pipeline(raw, &[], &words, false), before);
        }

        #[test]
        fn output_is_unchanged_for_users_with_no_personalisation_at_all() {
            let raw = "So uhm I was thinking uh about this";
            assert_eq!(
                pipeline(raw, &[], &[], false),
                filter_transcription_output(raw, "en", &None)
            );
        }

        #[test]
        fn an_empty_transcription_survives_the_whole_pipeline() {
            assert_eq!(pipeline("", &[("a", "b")], &["c"], false), "");
        }

        #[test]
        fn the_pipeline_is_idempotent() {
            let pairs = [("andy plus", "Handy Plus")];
            let once = pipeline("andy plus is good", &pairs, &["Handy Plus"], false);
            let twice = pipeline(&once, &pairs, &["Handy Plus"], false);
            assert_eq!(once, twice, "a second pass changed the text");
        }
    }
}
