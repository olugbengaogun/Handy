//! Building the Whisper decode prompt from the user's dictionary.
//!
//! Whisper accepts an `initial_prompt` that biases decoding toward preferred
//! spellings. It has a hard limit that is easy to miss: **only the last 224
//! tokens of the prompt are consumed, and everything before that is silently
//! discarded.** Tokens nearer the end also exert more influence than tokens
//! near the start.
//!
//! Handy Plus previously built the prompt with `terms.join(", ")` over an
//! unbounded dictionary. That is fine for a handful of words and quietly
//! degrades as the list grows: past roughly 150–200 terms the *earliest*
//! entries stop reaching the decoder at all, with nothing to tell the user.
//! Adding more words eventually makes the feature worse rather than better.
//!
//! This module makes the limit explicit: it fits terms into a conservative
//! budget, keeps the ones most likely to matter, and reports what it dropped so
//! the truncation can be surfaced instead of being invisible.

/// Token budget for the assembled prompt.
///
/// Whisper's own cap is 224; this leaves headroom so an estimate that runs a
/// little low can never push real terms off the front.
pub const WHISPER_PROMPT_TOKEN_BUDGET: usize = 180;

/// Bytes of prompt text assumed to cost one token.
///
/// Deliberately pessimistic. Common English words average closer to four
/// characters per token, but this prompt is made almost entirely of rare proper
/// nouns and product names, which byte-pair encoders split aggressively —
/// "ChargeBee" is several tokens, not one. Under-estimating here would silently
/// reintroduce the exact truncation this module exists to prevent.
const BYTES_PER_TOKEN: usize = 3;

/// Separator between terms in the emitted prompt.
const SEPARATOR: &str = ", ";

/// The outcome of fitting a dictionary into the prompt budget.
///
/// `Default` is "no prompt at all", which is what a non-Whisper model wants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WhisperPrompt {
    /// The prompt to hand the decoder, or `None` when nothing usable fit.
    pub text: Option<String>,
    /// How many terms made it in.
    pub included: usize,
    /// How many were dropped for lack of budget. Non-zero means the user's
    /// dictionary has outgrown what the model can be told about in one pass —
    /// worth surfacing in the UI rather than hiding.
    pub dropped: usize,
}

/// Estimate the token cost of a string, rounding up.
fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(BYTES_PER_TOKEN)
}

/// Assemble a Whisper `initial_prompt` from dictionary `terms`.
///
/// Selection rules, in order:
///
/// 1. Blank terms are skipped, and terms are de-duplicated case-insensitively —
///    a word that appears both in `custom_words` and as a correction pair's
///    `correct` side should not be charged to the budget twice.
/// 2. Terms are taken from the **end** of the list backwards. `effective_custom_words()`
///    appends as the user adds entries, so the tail is the newest and most
///    likely to be relevant to what they are dictating now.
/// 3. The selected terms are emitted in their original relative order, which
///    puts the newest term **last** — the position Whisper weights most.
///
/// Returns `included == 0` and `text == None` for an empty or unusable list, so
/// the caller can skip attaching a run extension entirely.
pub fn build_whisper_initial_prompt<S: AsRef<str>>(terms: &[S]) -> WhisperPrompt {
    // Deduplicate while preserving order, keeping the *first* occurrence so the
    // ordering above still means what it says.
    let mut seen: Vec<String> = Vec::new();
    let mut unique: Vec<&str> = Vec::new();
    for term in terms {
        let term = term.as_ref().trim();
        if term.is_empty() {
            continue;
        }
        let key = term.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        unique.push(term);
    }

    if unique.is_empty() {
        return WhisperPrompt {
            text: None,
            included: 0,
            dropped: 0,
        };
    }

    // Walk backwards from the newest, accumulating until the budget is spent.
    let mut budget = WHISPER_PROMPT_TOKEN_BUDGET;
    let mut take_from = unique.len();
    for (index, term) in unique.iter().enumerate().rev() {
        let mut cost = estimate_tokens(term);
        if take_from < unique.len() {
            cost += estimate_tokens(SEPARATOR);
        }
        if cost > budget {
            break;
        }
        budget -= cost;
        take_from = index;
    }

    let included = &unique[take_from..];
    if included.is_empty() {
        // A single term longer than the whole budget. Nothing sane to send.
        return WhisperPrompt {
            text: None,
            included: 0,
            dropped: unique.len(),
        };
    }

    WhisperPrompt {
        text: Some(included.join(SEPARATOR)),
        included: included.len(),
        dropped: unique.len() - included.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(n: usize, prefix: &str) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}{i}")).collect()
    }

    #[test]
    fn empty_input_produces_no_prompt() {
        let out = build_whisper_initial_prompt::<String>(&[]);
        assert_eq!(out.text, None);
        assert_eq!(out.included, 0);
        assert_eq!(out.dropped, 0);
    }

    #[test]
    fn blank_and_whitespace_terms_are_skipped() {
        let out = build_whisper_initial_prompt(&["", "   ", "Handy"]);
        assert_eq!(out.text.as_deref(), Some("Handy"));
        assert_eq!(out.included, 1);
    }

    #[test]
    fn a_small_dictionary_is_emitted_in_order_and_unchanged() {
        // The common case must behave exactly as the old `join(", ")` did.
        let out = build_whisper_initial_prompt(&["Handy", "ChargeBee", "cjpais"]);
        assert_eq!(out.text.as_deref(), Some("Handy, ChargeBee, cjpais"));
        assert_eq!(out.dropped, 0);
    }

    #[test]
    fn duplicates_are_removed_case_insensitively() {
        let out = build_whisper_initial_prompt(&["Handy", "handy", "HANDY", "Plus"]);
        assert_eq!(out.text.as_deref(), Some("Handy, Plus"));
        assert_eq!(out.included, 2);
    }

    #[test]
    fn terms_are_trimmed() {
        let out = build_whisper_initial_prompt(&["  Handy  "]);
        assert_eq!(out.text.as_deref(), Some("Handy"));
    }

    #[test]
    fn a_large_dictionary_is_truncated_rather_than_silently_overflowing() {
        let all = terms(500, "term");
        let out = build_whisper_initial_prompt(&all);
        assert!(out.dropped > 0, "expected truncation, got {out:?}");
        assert_eq!(out.included + out.dropped, 500);

        let text = out.text.expect("some terms should fit");
        assert!(
            estimate_tokens(&text) <= WHISPER_PROMPT_TOKEN_BUDGET,
            "prompt of {} estimated tokens exceeds the {} budget",
            estimate_tokens(&text),
            WHISPER_PROMPT_TOKEN_BUDGET
        );
    }

    #[test]
    fn truncation_keeps_the_newest_terms_and_puts_them_last() {
        let all = terms(500, "term");
        let out = build_whisper_initial_prompt(&all);
        let text = out.text.unwrap();

        // The newest term is the most influential position: the very end.
        assert!(
            text.ends_with("term499"),
            "newest term should be last, got tail: {:?}",
            &text[text.len().saturating_sub(40)..]
        );
        // The oldest terms are the ones sacrificed.
        assert!(
            !text.contains("term0,"),
            "oldest term should have been dropped"
        );
    }

    #[test]
    fn a_dictionary_that_exactly_fits_drops_nothing() {
        // Ten short terms are nowhere near the budget.
        let all = terms(10, "t");
        let out = build_whisper_initial_prompt(&all);
        assert_eq!(out.dropped, 0);
        assert_eq!(out.included, 10);
    }

    #[test]
    fn a_single_term_larger_than_the_budget_yields_no_prompt() {
        let huge = "x".repeat(WHISPER_PROMPT_TOKEN_BUDGET * BYTES_PER_TOKEN + 100);
        let out = build_whisper_initial_prompt(&[huge]);
        assert_eq!(out.text, None);
        assert_eq!(out.included, 0);
        assert_eq!(out.dropped, 1);
    }

    #[test]
    fn one_oversized_term_does_not_block_the_smaller_ones_after_it() {
        let huge = "x".repeat(WHISPER_PROMPT_TOKEN_BUDGET * BYTES_PER_TOKEN + 100);
        let out = build_whisper_initial_prompt(&[huge, "Handy".to_string()]);
        // Walking from the newest, "Handy" fits and the giant term does not.
        assert_eq!(out.text.as_deref(), Some("Handy"));
        assert_eq!(out.dropped, 1);
    }

    #[test]
    fn multibyte_terms_are_costed_by_bytes_not_chars() {
        // Budget accounting must not be fooled by non-ASCII scripts.
        let all: Vec<String> = (0..500).map(|i| format!("日本語{i}")).collect();
        let out = build_whisper_initial_prompt(&all);
        let text = out.text.unwrap();
        assert!(estimate_tokens(&text) <= WHISPER_PROMPT_TOKEN_BUDGET);
    }

    #[test]
    fn separator_cost_is_accounted_for() {
        // Many tiny terms: separators dominate, so the count must be well under
        // what naive per-term costing would allow.
        let all = terms(500, "");
        let out = build_whisper_initial_prompt(&all);
        let text = out.text.unwrap();
        assert!(estimate_tokens(&text) <= WHISPER_PROMPT_TOKEN_BUDGET);
    }
}
