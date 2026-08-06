//! Protected values: the facts a cleanup pass is never allowed to change.
//!
//! Optional LLM post-processing is the only stage in the pipeline that can
//! *invent* text, and whatever it returns is pasted into whatever the user had
//! focused. [`super::diff::is_plausible_cleanup`] already rejects output that
//! strays too far from the transcript overall, but that guard is a *ratio*: it
//! measures how much of the text changed, not how much it mattered. Changing
//! one token out of forty is well inside its budget — and if that token is an
//! amount of money, it is the single most damaging edit the app can make.
//!
//! The motivating case is real: the user says "eighty thousand naira", and a
//! confused cleanup writes `₦18,000`. It reads perfectly. Nothing in a
//! word-distance metric notices. The user pastes it into a message and sends it.
//!
//! This module adds a second, orthogonal gate. It never rewrites anything; it
//! only answers one question — *did a stated fact survive?* — and lets the
//! caller keep the raw transcription when the answer is no.
//!
//! ## The rules, and why each one is shaped this way
//!
//! 1. **Values are compared by value, not by spelling.** `80k`, `80,000` and
//!    "eighty thousand" all normalise to the same digits, so ordinary inverse
//!    text normalisation ("eighty thousand naira" → `₦80,000`) is recognised as
//!    *preserving* the number rather than changing it. Formatting numbers is
//!    exactly what cleanup is for; a gate that forbade it would be rejecting the
//!    good case.
//!
//! 2. **A run of number words is matched as a whole, against alternative
//!    readings.** "twenty twenty-six" is either two numbers or the year `2026`,
//!    and both are legitimate. So a run contributes a small set of candidate
//!    readings and is satisfied when *any one* of them survives intact.
//!
//!    This replaces an earlier design that let a spelled-out value match any
//!    cleaned number it was a prefix or suffix of. That rule was worse than
//!    useless: `180000` ends with `80000`, so "eighty thousand" → `₦180,000`
//!    would have passed — reproducing the exact corruption this module exists to
//!    stop. Matching whole readings by equality closes that hole by
//!    construction.
//!
//! 3. **Only a substitution is fatal.** If a value from the original is gone and
//!    the cleaned text introduced no value that was not already there, the
//!    cleanup is accepted. Real speech is full of self-correction — "five, no,
//!    six of them" legitimately becomes "six of them" — and rejecting those
//!    would silently disable cleanup for a large slice of ordinary dictation.
//!    What is never acceptable is a value *disappearing while a different one
//!    appears*, which is the ₦80,000 → ₦18,000 signature.
//!
//! 4. **Anything ambiguous is not extracted at all.** Version strings (`v1.2.3`),
//!    clock times (`4:40`), absurdly long digit runs and non-English number words
//!    parse to nothing and therefore constrain nothing. Dictated digit strings
//!    ("nine zero two, five five five…") are deliberately exempt too: how a
//!    listener groups them is genuinely ambiguous, so the gate has no business
//!    ruling on it. The module fails open by construction — what it cannot
//!    understand, it cannot reject.
//!
//! 5. **URLs and email addresses must survive verbatim.** Unlike numbers there
//!    is no legitimate cleanup that deletes one, and a half-rewritten link is
//!    worse than no link.
//!
//! ## Deliberate non-goals
//!
//! * **Invention is not policed here.** A cleanup that preserves every stated
//!   number and adds an unrelated one passes this gate; catching that is the
//!   divergence guard's job, and duplicating it here would only add false
//!   rejections.
//! * **Units and currency are not checked**, only magnitude. `₦80,000` becoming
//!   `$80,000` is invisible to this module.
//! * **English only.** Numbers spelled out in any other language produce no
//!   values, so the gate silently does less rather than doing damage.
//!
//! Because the gate can only ever *reject*, its worst case is that the user
//! receives the raw transcription — an outcome the pipeline already produces
//! whenever post-processing is disabled, fails, or diverges.

use std::collections::HashSet;

/// Longest run of digits treated as a quantity.
///
/// Past this it is an identifier — a licence key, a hash, a bare account
/// string — and comparing it as a number is meaningless. Skipping it costs
/// nothing: an unparsed token constrains nothing (rule 4).
const MAX_DIGITS: usize = 30;

/// Shortest run of single-digit number words treated as dictated digits rather
/// than as quantities. Two is too eager ("one two" is usually counting), and
/// phone numbers and account numbers always run longer than this.
const DIGIT_RUN_EXEMPTION: usize = 3;

/// One thing the cleaned text has to account for.
#[derive(Debug, PartialEq, Eq)]
enum Requirement {
    /// A number the source wrote as digits. It was heard as a number and must
    /// survive as that exact number.
    Exact(String),
    /// A run of number words, as alternative readings. Satisfied when every
    /// value of at least one reading is present.
    AnyReading(Vec<Vec<String>>),
}

/// Everything protected on one side of the comparison.
#[derive(Debug, Default)]
struct Protected {
    /// What the cleaned text must account for. Only built for the original.
    requirements: Vec<Requirement>,
    /// Every value any reading of this text could yield. Used both to satisfy
    /// the other side's requirements and to detect newly introduced values.
    values: HashSet<String>,
    /// Lowercased URLs, trailing punctuation removed.
    urls: Vec<String>,
    /// Lowercased email addresses.
    emails: Vec<String>,
}

/// Does `cleaned` preserve every protected value stated in `original`?
///
/// Returns `true` when the cleanup may be accepted. See the module docs for the
/// rules; the short version is that numbers may be reformatted or dropped but
/// never swapped, and links may not vanish.
pub fn preserves_protected_values(original: &str, cleaned: &str) -> bool {
    let before = extract(original);

    // Nothing worth protecting — the divergence guard is the only relevant
    // check, and it has already run.
    if before.requirements.is_empty() && before.urls.is_empty() && before.emails.is_empty() {
        return true;
    }

    let after = extract(cleaned);

    // Rule 5: a link that vanished is never a cleanup.
    if before.urls.iter().any(|url| !after.urls.contains(url)) {
        return false;
    }
    if before
        .emails
        .iter()
        .any(|email| !after.emails.contains(email))
    {
        return false;
    }

    let all_satisfied = before
        .requirements
        .iter()
        .all(|requirement| is_satisfied(requirement, &after.values));

    if all_satisfied {
        return true;
    }

    // Rule 3: losing a value is tolerable; trading one for another is not.
    let introduced = after
        .values
        .iter()
        .any(|value| !before.values.contains(value));

    !introduced
}

fn is_satisfied(requirement: &Requirement, present: &HashSet<String>) -> bool {
    match requirement {
        Requirement::Exact(value) => present.contains(value),
        Requirement::AnyReading(readings) => readings
            .iter()
            .any(|reading| reading.iter().all(|value| present.contains(value))),
    }
}

/// Pull every protected fact out of one side of the comparison.
fn extract(text: &str) -> Protected {
    let mut found = Protected::default();
    // Number words accumulate across tokens, so the run is tracked between
    // iterations and closed when anything else interrupts it.
    let mut run = WordRun::default();

    for raw in text.split_whitespace() {
        let token = trim_edges(raw);
        if token.is_empty() {
            run.close(&mut found);
            continue;
        }

        let lower = token.to_lowercase();

        if is_url(&lower) {
            run.close(&mut found);
            if !found.urls.contains(&lower) {
                found.urls.push(lower);
            }
            continue;
        }
        if is_email(&lower) {
            run.close(&mut found);
            if !found.emails.contains(&lower) {
                found.emails.push(lower);
            }
            continue;
        }

        if let Some(value) = parse_written_number(token) {
            run.close(&mut found);
            found.values.insert(value.clone());
            found.requirements.push(Requirement::Exact(value));
            continue;
        }

        // A hyphenated compound ("twenty-six") is two number words.
        let mut consumed_all = true;
        let mut consumed_any = false;
        for part in lower.split(['-', '\u{2013}', '\u{2014}']) {
            if part.is_empty() {
                continue;
            }
            if run.push(part) {
                consumed_any = true;
            } else {
                consumed_all = false;
            }
        }
        if !consumed_all || !consumed_any {
            run.close(&mut found);
        }
    }

    run.close(&mut found);
    found
}

/// Trim non-alphanumeric characters from both ends of a token.
///
/// Currency symbols, brackets and sentence punctuation go; anything inside the
/// token (separators, the dots of a URL, an `@`) stays. Applied identically to
/// both sides, so it can never introduce an asymmetry.
fn trim_edges(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}

fn is_url(lower: &str) -> bool {
    lower.contains("://") || lower.starts_with("www.")
}

fn is_email(lower: &str) -> bool {
    if lower.contains("://") {
        return false;
    }
    match lower.split_once('@') {
        Some((user, domain)) => {
            !user.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        }
        None => false,
    }
}

/// Parse a token written as digits into its normalised value.
///
/// Handles thousands separators, a single decimal point, `k`/`m`/`b`
/// multipliers and ordinal suffixes. Returns `None` for anything that is not
/// unambiguously one quantity — version strings and clock times contain
/// characters this refuses, which is how they end up constraining nothing.
fn parse_written_number(token: &str) -> Option<String> {
    let split = token
        .char_indices()
        .find(|(_, c)| c.is_alphabetic())
        .map(|(i, _)| i)
        .unwrap_or(token.len());
    let (digits, suffix) = token.split_at(split);

    if !digits.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    if digits
        .chars()
        .any(|c| !c.is_ascii_digit() && c != ',' && c != '.' && c != '_')
    {
        return None;
    }

    let cleaned: String = digits.chars().filter(|c| *c != ',' && *c != '_').collect();
    let mut parts = cleaned.split('.');
    let integer = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    // Two or more dots: a version string, an IP address, an ellipsis. Not a
    // quantity — leave it alone (rule 4).
    if parts.next().is_some() {
        return None;
    }
    if integer.is_empty() || integer.len() + fraction.len() > MAX_DIGITS {
        return None;
    }

    let zeros = multiplier_zeros(&suffix.to_lowercase())?.unwrap_or(0);

    // A fraction only survives if a multiplier can absorb it: "1.5k" is 1500,
    // but "1.5" on its own is compared as written.
    if fraction.len() > zeros {
        if zeros > 0 {
            return None;
        }
        return Some(normalise_digits(&format!("{}.{}", integer, fraction)));
    }

    let mut value = String::with_capacity(integer.len() + zeros);
    value.push_str(integer);
    value.push_str(fraction);
    for _ in 0..(zeros - fraction.len()) {
        value.push('0');
    }
    Some(normalise_digits(&value))
}

/// How many zeros a trailing multiplier is worth.
///
/// `Some(None)` means "no multiplier, and the suffix is harmless" (an ordinal,
/// a plural, or no suffix at all). `None` means the suffix is not something this
/// module understands, so the token is not a quantity it may reason about.
fn multiplier_zeros(suffix: &str) -> Option<Option<usize>> {
    match suffix {
        "" => Some(None),
        "k" => Some(Some(3)),
        "m" | "mn" => Some(Some(6)),
        "b" | "bn" => Some(Some(9)),
        "st" | "nd" | "rd" | "th" | "s" => Some(None),
        _ => None,
    }
}

/// Strip leading zeros so `007` and `7` compare equal.
///
/// Applied to both sides identically, so a leading zero that is genuinely part
/// of the text still matches itself.
fn normalise_digits(value: &str) -> String {
    let (integer, fraction) = match value.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (value, None),
    };
    let trimmed = integer.trim_start_matches('0');
    let integer = if trimmed.is_empty() { "0" } else { trimmed };
    match fraction {
        Some(fraction) => format!("{}.{}", integer, fraction),
        None => integer.to_string(),
    }
}

/// Accumulator for a contiguous run of English number words.
///
/// Deliberately English-only. Every other language produces no values, which by
/// rule 4 means the gate cannot reject on their behalf — a limitation, but a
/// safe one.
#[derive(Default)]
struct WordRun {
    /// Numbers already closed off within this run — "twenty twenty-six"
    /// completes `20` before it starts on `26`.
    completed: Vec<u64>,
    /// The 0–99 part currently being assembled.
    small: u64,
    /// Whether `small` holds anything, distinguishing "zero" from "unset".
    small_set: bool,
    /// The hundreds part of the group being assembled.
    hundreds: u64,
    /// Groups already closed by a scale word (thousand, million, …).
    total: u64,
    /// Whether anything has been accumulated into the number in progress.
    any: bool,
    /// Whether every word in this run was a single digit, which makes it
    /// dictated digits rather than a quantity.
    all_single_digits: bool,
    /// How many number words the run has consumed.
    word_count: usize,
}

impl WordRun {
    /// Feed one lowercased word. Returns `true` if it was part of a number.
    fn push(&mut self, word: &str) -> bool {
        if word == "and" {
            // Only a connector *inside* a number ("one hundred and five"), never
            // the start of one.
            return self.any || !self.completed.is_empty();
        }

        if self.word_count == 0 {
            self.all_single_digits = true;
        }

        if let Some(value) = small_word_value(word) {
            // "twenty" + "six" is one number; "twenty" + "twenty" is two. Only a
            // bare tens word followed by a unit continues the number in progress.
            let continues_tens = self.small_set
                && matches!(self.small, 20 | 30 | 40 | 50 | 60 | 70 | 80 | 90)
                && value < 10;
            if self.small_set && !continues_tens {
                // "twenty twenty" is two numbers, not forty.
                self.close_current();
            }
            if self.small_set {
                self.small += value;
            } else {
                self.small = value;
                self.small_set = true;
            }
            self.any = true;
            self.all_single_digits &= value < 10;
            self.word_count += 1;
            return true;
        }

        let scaled = match word {
            "hundred" => {
                let base = if self.small_set && self.small > 0 {
                    self.small
                } else {
                    1
                };
                self.hundreds = self.hundreds.saturating_add(base.saturating_mul(100));
                self.small = 0;
                self.small_set = false;
                self.any = true;
                true
            }
            "thousand" => self.scale(1_000),
            "million" => self.scale(1_000_000),
            "billion" => self.scale(1_000_000_000),
            _ => false,
        };

        if scaled {
            self.all_single_digits = false;
            self.word_count += 1;
        }
        scaled
    }

    fn scale(&mut self, factor: u64) -> bool {
        let group = self.hundreds + if self.small_set { self.small } else { 0 };
        let group = if group == 0 { 1 } else { group };
        self.total = self.total.saturating_add(group.saturating_mul(factor));
        self.hundreds = 0;
        self.small = 0;
        self.small_set = false;
        self.any = true;
        true
    }

    /// Close the number in progress, if any, and start a new one.
    fn close_current(&mut self) {
        if let Some(value) = self.current() {
            self.completed.push(value);
        }
        self.small = 0;
        self.small_set = false;
        self.hundreds = 0;
        self.total = 0;
        self.any = false;
    }

    fn current(&self) -> Option<u64> {
        if !self.any {
            return None;
        }
        Some(self.total + self.hundreds + if self.small_set { self.small } else { 0 })
    }

    /// End the run, contributing its readings, and reset.
    fn close(&mut self, found: &mut Protected) {
        self.close_current();
        let values = std::mem::take(&mut self.completed);
        let all_single_digits = self.all_single_digits;
        let word_count = self.word_count;
        *self = WordRun::default();

        if values.is_empty() {
            return;
        }

        // Rule 4: a dictated digit string ("nine zero two, five five five…")
        // can be grouped a dozen ways and no grouping is more correct than
        // another. Ruling on it would only produce false rejections.
        if all_single_digits && word_count >= DIGIT_RUN_EXEMPTION {
            return;
        }

        let individually: Vec<String> = values
            .iter()
            .map(|value| normalise_digits(&value.to_string()))
            .collect();

        let mut readings = vec![individually.clone()];

        // Rule 2: adjacent spoken numbers may be one written number —
        // "nineteen eighty-four" is 1984.
        if values.len() > 1 {
            let joined: String = individually.concat();
            if joined.len() <= MAX_DIGITS {
                readings.push(vec![normalise_digits(&joined)]);
            }
        }

        for reading in &readings {
            for value in reading {
                found.values.insert(value.clone());
            }
        }
        found.requirements.push(Requirement::AnyReading(readings));
    }
}

/// Value of a single number word below one hundred, cardinal or ordinal.
///
/// Ordinals are included because "on the eighth" becoming "on the 18th" is a
/// date silently moving by ten days — the same class of damage as an amount
/// changing. They bring in words that are also ordinary discourse markers
/// ("first of all", "wait a second"), which is tolerable: both sides are parsed
/// identically, and a value that merely *disappears* is allowed by rule 3. Only
/// a swap is refused, and "first of all" does not get swapped for a number.
fn small_word_value(word: &str) -> Option<u64> {
    let value = match word {
        "zero" | "oh" | "nought" => 0,
        "one" | "first" => 1,
        "two" | "second" => 2,
        "three" | "third" => 3,
        "four" | "fourth" => 4,
        "five" | "fifth" => 5,
        "six" | "sixth" => 6,
        "seven" | "seventh" => 7,
        "eight" | "eighth" => 8,
        "nine" | "ninth" => 9,
        "ten" | "tenth" => 10,
        "eleven" | "eleventh" => 11,
        "twelve" | "twelfth" => 12,
        "thirteen" | "thirteenth" => 13,
        "fourteen" | "fourteenth" => 14,
        "fifteen" | "fifteenth" => 15,
        "sixteen" | "sixteenth" => 16,
        "seventeen" | "seventeenth" => 17,
        "eighteen" | "eighteenth" => 18,
        "nineteen" | "nineteenth" => 19,
        "twenty" | "twentieth" => 20,
        "thirty" | "thirtieth" => 30,
        "forty" | "fortieth" => 40,
        "fifty" | "fiftieth" => 50,
        "sixty" | "sixtieth" => 60,
        "seventy" | "seventieth" => 70,
        "eighty" | "eightieth" => 80,
        "ninety" | "ninetieth" => 90,
        _ => return None,
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(text: &str) -> Vec<String> {
        let mut found: Vec<String> = extract(text).values.into_iter().collect();
        found.sort();
        found
    }

    // ── the motivating failure ────────────────────────────────────────────

    #[test]
    fn a_swapped_amount_is_rejected() {
        assert!(!preserves_protected_values(
            "it costs 80k for the term",
            "It costs ₦18,000 for the term."
        ));
    }

    #[test]
    fn spelling_out_an_amount_is_preserved() {
        assert!(preserves_protected_values(
            "it costs eighty thousand naira for the term",
            "It costs ₦80,000 for the term."
        ));
    }

    /// The red-team case that killed the first design: `180000` ends with
    /// `80000`, so a prefix/suffix rule accepted a corrupted amount.
    #[test]
    fn a_prepended_digit_is_rejected() {
        assert!(!preserves_protected_values(
            "it costs eighty thousand naira",
            "It costs ₦180,000."
        ));
    }

    /// Same flaw from the other end: `800000` starts with `80000`.
    #[test]
    fn an_order_of_magnitude_shift_is_rejected() {
        assert!(!preserves_protected_values(
            "it costs eighty thousand naira",
            "It costs ₦800,000."
        ));
    }

    #[test]
    fn ordinal_drift_is_rejected() {
        assert!(!preserves_protected_values(
            "the meeting is on the eighth",
            "The meeting is on the 18th."
        ));
    }

    // ── legitimate cleanups must survive ──────────────────────────────────

    #[test]
    fn a_year_read_as_two_numbers_is_preserved() {
        assert!(preserves_protected_values(
            "back in twenty twenty-six",
            "Back in 2026."
        ));
    }

    #[test]
    fn nineteen_eighty_four_is_preserved() {
        assert!(preserves_protected_values(
            "it was nineteen eighty-four",
            "It was 1984."
        ));
    }

    #[test]
    fn compound_ordinals_survive_being_written_as_digits() {
        assert!(preserves_protected_values(
            "the meeting is on the twenty-first",
            "The meeting is on the 21st."
        ));
    }

    /// Ordinal words double as discourse markers, so losing one must not be
    /// treated as a corrupted fact — rule 3 covers it.
    #[test]
    fn a_dropped_discourse_ordinal_is_not_a_corruption() {
        assert!(preserves_protected_values(
            "first of all um it costs eighty thousand",
            "It costs ₦80,000."
        ));
    }

    #[test]
    fn a_self_correction_may_drop_a_number() {
        assert!(preserves_protected_values(
            "there were five no six of them",
            "There were six of them."
        ));
    }

    #[test]
    fn multipliers_and_separators_are_the_same_value() {
        assert!(preserves_protected_values("about 80k", "About 80,000."));
        assert!(preserves_protected_values("about 1.5k", "About 1,500."));
        assert!(preserves_protected_values("about 2m", "About 2,000,000."));
    }

    #[test]
    fn digits_may_be_spelled_back_out() {
        assert!(preserves_protected_values(
            "i need 2026 copies",
            "I need two thousand and twenty-six copies."
        ));
    }

    /// How a listener groups dictated digits is genuinely ambiguous, so the gate
    /// must not rule on it (rule 4).
    #[test]
    fn dictated_digit_strings_are_exempt() {
        assert!(preserves_protected_values(
            "call nine zero two five five five six seven three one",
            "Call 902 555 6731."
        ));
    }

    /// Clock times contain a colon, which `parse_written_number` refuses, so
    /// "twenty to five" → "4:40" constrains nothing.
    #[test]
    fn clock_times_do_not_constrain() {
        assert!(preserves_protected_values(
            "let us meet at twenty to five",
            "Let us meet at 4:40."
        ));
    }

    #[test]
    fn version_strings_do_not_constrain() {
        assert!(preserves_protected_values(
            "upgrade to v1.2.3 today",
            "Upgrade to v1.2.4 today."
        ));
    }

    #[test]
    fn text_without_protected_values_is_always_accepted() {
        assert!(preserves_protected_values(
            "so um i was thinking we could go",
            "So I was thinking we could go."
        ));
    }

    #[test]
    fn punctuation_and_casing_alone_never_reject() {
        assert!(preserves_protected_values(
            "i paid 40 pounds on the 3rd",
            "I paid 40 pounds on the 3rd."
        ));
    }

    // ── links ─────────────────────────────────────────────────────────────

    #[test]
    fn a_dropped_url_is_rejected() {
        assert!(!preserves_protected_values(
            "the docs are at https://handy.computer/docs ok",
            "The docs are online, okay."
        ));
    }

    #[test]
    fn a_preserved_url_is_accepted() {
        assert!(preserves_protected_values(
            "the docs are at https://handy.computer/docs ok",
            "The docs are at https://handy.computer/docs, okay."
        ));
    }

    #[test]
    fn a_dropped_email_is_rejected() {
        assert!(!preserves_protected_values(
            "mail me at ade@example.com please",
            "Mail me please."
        ));
    }

    #[test]
    fn an_added_link_is_fine() {
        assert!(preserves_protected_values(
            "see the docs",
            "See the docs at https://handy.computer/docs."
        ));
    }

    // ── stated non-goals, pinned so a change of mind is deliberate ─────────

    #[test]
    fn invention_alongside_preserved_values_is_not_this_gates_job() {
        assert!(preserves_protected_values(
            "it costs 80k",
            "It costs ₦80,000, and about 5,000 more in fees."
        ));
    }

    #[test]
    fn currency_is_not_checked_only_magnitude() {
        assert!(preserves_protected_values(
            "it costs 80k naira",
            "It costs $80,000."
        ));
    }

    // ── parsing ───────────────────────────────────────────────────────────

    #[test]
    fn number_words_accumulate_correctly() {
        assert_eq!(values("eighty thousand"), vec!["80000"]);
        assert_eq!(values("one hundred and twenty five"), vec!["125"]);
        assert_eq!(values("two million"), vec!["2000000"]);
        // Two numbers, plus the joined reading.
        assert_eq!(values("twenty twenty-six"), vec!["20", "2026", "26"]);
    }

    #[test]
    fn digit_forms_normalise_to_one_value() {
        assert_eq!(parse_written_number("80,000").as_deref(), Some("80000"));
        assert_eq!(parse_written_number("80k").as_deref(), Some("80000"));
        assert_eq!(parse_written_number("1.5k").as_deref(), Some("1500"));
        assert_eq!(parse_written_number("8th").as_deref(), Some("8"));
        assert_eq!(parse_written_number("007").as_deref(), Some("7"));
        assert_eq!(parse_written_number("3.14").as_deref(), Some("3.14"));
    }

    #[test]
    fn ambiguous_tokens_parse_to_nothing() {
        assert_eq!(parse_written_number("1.2.3"), None);
        assert_eq!(parse_written_number("4:40"), None);
        assert_eq!(parse_written_number("abc"), None);
        assert_eq!(parse_written_number("12xyz"), None);
        // Longer than MAX_DIGITS.
        assert_eq!(parse_written_number(&"1".repeat(31)), None);
    }

    #[test]
    fn empty_and_whitespace_inputs_do_not_panic() {
        assert!(preserves_protected_values("", ""));
        assert!(preserves_protected_values("   ", "80,000"));
        assert!(preserves_protected_values("", "80,000"));
        // Emptying a transcript entirely is a *deletion*, which rule 3 permits
        // — and which `is_plausible_cleanup` has already refused outright
        // before this gate is ever consulted. Duplicating that check here would
        // only make two places responsible for one decision.
        assert!(preserves_protected_values("80k", ""));
        assert!(!super::super::diff::is_plausible_cleanup(
            "80k",
            "",
            super::super::diff::DEFAULT_MAX_DIVERGENCE
        ));
    }

    #[test]
    fn unicode_input_does_not_panic() {
        assert!(preserves_protected_values("￥80,000 の話", "￥80,000 の話"));
        assert!(preserves_protected_values("—— …… ‽", "—— …… ‽"));
        assert!(preserves_protected_values("八十千", "八十千"));
    }

    /// A non-English transcript produces no values, so the gate stays silent
    /// rather than guessing (rule 4).
    #[test]
    fn non_english_number_words_do_not_constrain() {
        assert!(preserves_protected_values(
            "cuesta ochenta mil nairas",
            "Cuesta 18.000 nairas."
        ));
    }

    #[test]
    fn the_gate_is_deterministic_and_idempotent() {
        let original = "it costs eighty thousand naira at https://x.example/a";
        let cleaned = "It costs ₦80,000 at https://x.example/a.";
        for _ in 0..3 {
            assert!(preserves_protected_values(original, cleaned));
        }
    }
}
