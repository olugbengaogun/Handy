//! Phonetic matching for the custom-word corrector.
//!
//! The fuzzy matcher in [`super::text`] scores a candidate against the user's
//! dictionary using edit distance plus a phonetic signal. That phonetic signal
//! has always been Soundex, which is a poor fit for the job: it discards every
//! vowel after the first letter, truncates to a fixed four characters, and was
//! designed in 1918 for American surnames on census cards. It is simultaneously
//! too loose (unrelated words collide) and too blind (real pronunciation
//! variants of a product name do not).
//!
//! Double Metaphone models a great deal more of English pronunciation, handles
//! names of non-English origin, and returns **two** codes — a primary and an
//! alternate — so a word with two plausible pronunciations can match either.
//!
//! ## Why this is opt-in
//!
//! Changing the phonetic algorithm changes which candidates clear
//! `word_correction_threshold`, and that threshold's tuned value (0.18) was
//! chosen against Soundex's behaviour. Switching silently would alter matching
//! for every existing user's dictionary in a direction nobody has measured.
//! So the strength is exposed as a setting that defaults to the existing
//! behaviour, and the evaluation harness (`scripts/wer-bench.ts`) exists to make
//! the decision with data instead of taste.

use once_cell::sync::Lazy;
use rphonetic::{DoubleMetaphone, Encoder};

/// Shared encoder. Construction parses internal tables, so it is done once.
///
/// Four characters is the Apache commons-codec default and the length the
/// algorithm's published behaviour is defined against.
static ENCODER: Lazy<DoubleMetaphone> = Lazy::new(DoubleMetaphone::default);

/// How strongly two strings agree phonetically.
///
/// Ordered weakest to strongest so comparisons read naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PhoneticAgreement {
    /// No phonetic relationship detected.
    None,
    /// The codes agree only via an alternate pronunciation. Real, but weaker
    /// evidence than a primary match.
    Alternate,
    /// Both strings produce the same primary code.
    Primary,
}

/// Double Metaphone is defined over ASCII letters. Anything else (digits, CJK,
/// accented text) has no meaningful code, and feeding it in produces junk that
/// would match other junk.
fn is_encodable(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphabetic())
}

/// Compare two strings phonetically using Double Metaphone.
///
/// Returns [`PhoneticAgreement::None`] for anything the algorithm cannot
/// meaningfully encode, so callers get a conservative answer rather than a
/// coincidental one.
pub fn agreement(a: &str, b: &str) -> PhoneticAgreement {
    if !is_encodable(a) || !is_encodable(b) {
        return PhoneticAgreement::None;
    }

    let a_primary = ENCODER.encode(a);
    let b_primary = ENCODER.encode(b);

    // An empty code means the encoder found nothing to work with; treating two
    // empties as a match would make unrelated inputs agree perfectly.
    if a_primary.is_empty() || b_primary.is_empty() {
        return PhoneticAgreement::None;
    }

    if a_primary == b_primary {
        return PhoneticAgreement::Primary;
    }

    let a_alt = ENCODER.encode_alternate(a);
    let b_alt = ENCODER.encode_alternate(b);

    // Cross-matching in both directions: "Smith" and "Schmidt" agree only when
    // one word's primary is compared against the other's alternate.
    let cross = (!a_alt.is_empty() && a_alt == b_primary)
        || (!b_alt.is_empty() && b_alt == a_primary)
        || (!a_alt.is_empty() && !b_alt.is_empty() && a_alt == b_alt);

    if cross {
        PhoneticAgreement::Alternate
    } else {
        PhoneticAgreement::None
    }
}

/// Multiplier applied to a candidate's edit-distance score.
///
/// Lower is a better match. Returning 1.0 leaves the score untouched.
///
/// The primary-match multiplier is deliberately identical to the value the
/// Soundex path has always used (0.3), so a pair that both algorithms agree on
/// scores exactly as it did before. Only the *set* of agreeing pairs changes,
/// and an alternate-only match is discounted because it is weaker evidence.
pub fn score_multiplier(agreement: PhoneticAgreement) -> f64 {
    match agreement {
        PhoneticAgreement::Primary => 0.3,
        PhoneticAgreement::Alternate => 0.45,
        PhoneticAgreement::None => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_words_agree_on_the_primary_code() {
        assert_eq!(agreement("handy", "handy"), PhoneticAgreement::Primary);
    }

    #[test]
    fn homophones_agree() {
        assert_eq!(agreement("smith", "smyth"), PhoneticAgreement::Primary);
    }

    #[test]
    fn a_real_mishearing_of_a_product_name_agrees() {
        // The case the custom-word feature exists for.
        assert_ne!(agreement("handy", "hendee"), PhoneticAgreement::None);
    }

    #[test]
    fn unrelated_words_do_not_agree() {
        assert_eq!(agreement("handy", "elephant"), PhoneticAgreement::None);
        assert_eq!(agreement("cat", "refrigerator"), PhoneticAgreement::None);
    }

    #[test]
    fn non_ascii_input_is_refused_rather_than_guessed_at() {
        assert_eq!(agreement("café", "cafe"), PhoneticAgreement::None);
        assert_eq!(agreement("你好", "你号"), PhoneticAgreement::None);
    }

    #[test]
    fn digits_and_mixed_alphanumerics_are_refused() {
        // "gpt4" has no phonetic code worth trusting; edit distance still
        // handles it in the caller.
        assert_eq!(agreement("gpt4", "gpt"), PhoneticAgreement::None);
        assert_eq!(agreement("123", "456"), PhoneticAgreement::None);
    }

    #[test]
    fn empty_input_never_agrees() {
        assert_eq!(agreement("", ""), PhoneticAgreement::None);
        assert_eq!(agreement("", "handy"), PhoneticAgreement::None);
    }

    #[test]
    fn agreement_is_symmetric() {
        for (a, b) in [("smith", "schmidt"), ("handy", "hendee"), ("cat", "dog")] {
            assert_eq!(
                agreement(a, b),
                agreement(b, a),
                "asymmetric result for {a}/{b}"
            );
        }
    }

    #[test]
    fn a_primary_match_scores_exactly_as_the_old_soundex_path_did() {
        // Pinned so the upgrade cannot silently re-tune
        // `word_correction_threshold` for pairs that already matched.
        assert_eq!(score_multiplier(PhoneticAgreement::Primary), 0.3);
    }

    #[test]
    fn an_alternate_match_is_weaker_and_no_match_is_neutral() {
        assert!(
            score_multiplier(PhoneticAgreement::Alternate)
                > score_multiplier(PhoneticAgreement::Primary)
        );
        assert_eq!(score_multiplier(PhoneticAgreement::None), 1.0);
    }

    #[test]
    fn agreement_strength_is_ordered() {
        assert!(PhoneticAgreement::Primary > PhoneticAgreement::Alternate);
        assert!(PhoneticAgreement::Alternate > PhoneticAgreement::None);
    }
}
