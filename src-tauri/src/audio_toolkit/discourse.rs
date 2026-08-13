//! Removal of spoken discourse markers ("you know", "I mean").
//!
//! The single-word filler filter in [`super::text`] handles interjections —
//! "um", "uh", "hmm". It cannot handle phrases, because its patterns are built
//! as `\b{word}\b[,.]?` with no awareness of clause boundaries: `\byou know\b`
//! matches inside "do you know the answer" and would leave "do the answer".
//!
//! This pass removes the *parenthetical* use of a marker while leaving its
//! literal use untouched, on one conservative signal:
//!
//! > A marker is removed only when it is immediately followed by a comma.
//!
//! That single rule does all the safety work. "do you know the answer" and
//! "I mean it" have no trailing comma and are never touched. The cost is
//! under-removal — "of course you know I had Chrome" keeps its marker, because
//! nothing in the punctuation distinguishes it from a literal "you know". The
//! trade is deliberate: a missed filler is invisible, a deleted verb changes
//! what the user said.
//!
//! Only `you know` and `I mean` are handled. `sort of` and `kind of` were
//! considered and rejected — "what kind of, uh, car" would collapse to "what
//! car", and a trailing-comma rule cannot protect a genuine noun modifier
//! because speech-to-text inserts commas mid-phrase.

/// Markers, as lowercase word sequences. Every marker is at least two words; a
/// single-word marker belongs in the filler list instead.
const MARKERS: &[&[&str]] = &[&["you", "know"], &["i", "mean"]];

/// Words that open a sentence and legitimately own their comma.
///
/// `X, you know, Y` is ambiguous. "thinking about, you know, finding" wants
/// both commas gone ("thinking about finding"); "Well, you know, the thing is"
/// wants one kept ("Well, the thing is"). The two are structurally identical,
/// so the preceding word decides — and only in first position, since "so" mid
/// sentence is an ordinary conjunction.
const SENTENCE_OPENERS: &[&str] = &[
    "well", "okay", "ok", "yeah", "yes", "right", "now", "so", "look", "see", "hey",
];

/// A whitespace-delimited token together with the whitespace preceding it.
struct Token<'a> {
    /// Whitespace immediately before this token. May contain newlines, which
    /// must survive: `corrections.rs` treats line breaks as hard boundaries.
    gap: &'a str,
    text: &'a str,
}

/// Splits into tokens, preserving the exact whitespace between them.
fn tokenize(text: &str) -> (Vec<Token<'_>>, &str) {
    let mut tokens = Vec::new();
    let mut gap_start = 0usize;
    let mut token_start: Option<usize> = None;

    for (offset, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                tokens.push(Token {
                    gap: &text[gap_start..start],
                    text: &text[start..offset],
                });
                gap_start = offset;
            }
        } else if token_start.is_none() {
            token_start = Some(offset);
        }
    }

    if let Some(start) = token_start {
        tokens.push(Token {
            gap: &text[gap_start..start],
            text: &text[start..],
        });
        return (tokens, "");
    }

    // Whatever trailed the final token is pure whitespace.
    (tokens, &text[gap_start..])
}

/// True when `token`, ignoring trailing commas, is exactly `word`.
fn core_is(token: &str, word: &str) -> bool {
    token.trim_end_matches(',').eq_ignore_ascii_case(word)
}

/// True when the token carries no trailing punctuation at all.
fn is_bare(token: &str) -> bool {
    !token.ends_with(|c: char| c.is_ascii_punctuation())
}

/// True when a kept token ends a sentence, so the next word begins one.
fn ends_sentence(token: &str) -> bool {
    token.ends_with(&['.', '!', '?'][..])
}

/// Uppercases the first character when it is lowercase, leaving the rest alone.
fn capitalize_first(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) if first.is_lowercase() => {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => token.to_string(),
    }
}

/// Does the marker of `words` start at `index`, punctuated as a parenthetical?
fn marker_matches(tokens: &[Token<'_>], index: usize, words: &[&str]) -> bool {
    if index + words.len() > tokens.len() {
        return false;
    }
    words.iter().enumerate().all(|(offset, word)| {
        let token = tokens[index + offset].text;
        if !core_is(token, word) {
            return false;
        }
        if offset + 1 == words.len() {
            // The comma is the whole signal that this use is parenthetical.
            token.ends_with(',')
        } else {
            // "you, know," is not the phrase.
            is_bare(token)
        }
    })
}

/// Removes parenthetical discourse markers from English text.
///
/// `lang` is matched on its base subtag, so `en-GB` and `en_US` both resolve to
/// English; every other language is returned untouched, matching how the filler
/// list in [`super::text`] is scoped per language.
///
/// Never returns empty for non-empty input: if everything were removable, the
/// original comes back. Emptying real speech is never a valid cleanup — the
/// same principle enforced by
/// [`is_plausible_cleanup`](super::diff::is_plausible_cleanup).
///
/// Idempotent: the output contains no removable marker, so a second pass is a
/// no-op.
pub fn remove_discourse_fillers(text: &str, lang: &str) -> String {
    let base_lang = lang.split(&['-', '_'][..]).next().unwrap_or(lang);
    if !base_lang.eq_ignore_ascii_case("en") {
        return text.to_string();
    }

    let (tokens, trailing_gap) = tokenize(text);

    // Surviving tokens, owned: a preceding token can lose its comma and a
    // following one can gain a capital.
    let mut kept: Vec<(&str, String)> = Vec::with_capacity(tokens.len());
    let mut index = 0usize;
    let mut removed_any = false;
    // Set when a removal leaves the next surviving token starting a sentence.
    let mut capitalize_next = false;
    // A removed marker takes its own leading gap with it. When that gap carried
    // a line break, the break has to be handed to the next surviving token or
    // two hard-bounded segments silently merge.
    let mut carried_gap: Option<&str> = None;

    while index < tokens.len() {
        let matched = MARKERS
            .iter()
            .copied()
            .find(|words| marker_matches(&tokens, index, words));

        let Some(words) = matched else {
            let mut text_out = tokens[index].text.to_string();
            if capitalize_next {
                text_out = capitalize_first(&text_out);
                capitalize_next = false;
            }
            let gap = carried_gap.take().unwrap_or(tokens[index].gap);
            kept.push((gap, text_out));
            index += 1;
            continue;
        };

        removed_any = true;

        // Preserve a line break that belonged to the marker being dropped. Every
        // gap inside the marker counts, not just the one in front of it: the
        // break can fall between "you" and "know,". The earliest wins, so
        // back-to-back markers keep the outermost boundary.
        if carried_gap.is_none() {
            if let Some(gap) = (0..words.len())
                .map(|offset| tokens[index + offset].gap)
                .find(|gap| gap.contains('\n'))
            {
                carried_gap = Some(gap);
            }
        }

        // A marker at the very start, or straight after a full stop, means the
        // next surviving word begins a sentence.
        let starts_sentence = match kept.last() {
            None => true,
            Some((_, previous)) => ends_sentence(previous),
        };
        if starts_sentence {
            capitalize_next = true;
        }

        // Is the last surviving token itself sentence-initial? That is what
        // decides whether an opener owns its comma — not whether it happens to
        // be the first word of the whole transcript.
        let previous_starts_sentence = kept.len() == 1
            || kept
                .len()
                .checked_sub(2)
                .and_then(|before| kept.get(before))
                .map_or(true, |(_, token)| ends_sentence(token));

        // Decide the fate of a comma on the last surviving token. Tracking the
        // last *kept* token rather than `index - 1` is what lets consecutive
        // markers collapse correctly in a single pass.
        if let Some((_, previous)) = kept.last_mut() {
            if previous.ends_with(',') {
                // Byte length of the token without its trailing commas. Commas
                // are ASCII, so this is always a char boundary — and truncating
                // in place avoids reassigning through the same borrow.
                let stem_len = previous.trim_end_matches(',').len();
                let owns_its_comma = previous_starts_sentence
                    && SENTENCE_OPENERS
                        .iter()
                        .any(|opener| previous[..stem_len].eq_ignore_ascii_case(opener));
                if !owns_its_comma {
                    previous.truncate(stem_len);
                }
            }
        }

        index += words.len();
    }

    if !removed_any {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    for (position, (gap, token)) in kept.iter().enumerate() {
        // The first surviving token starts the text, so a gap in front of it is
        // an artefact of whatever was removed — dropped rather than left as a
        // leading space. A line break is structure, not spacing, so it stays.
        if position > 0 || gap.contains('\n') {
            out.push_str(gap);
        }
        out.push_str(token);
    }
    out.push_str(trailing_gap);

    // Real speech went in; something must come out, even if it is the original.
    if out.trim().is_empty() && !text.trim().is_empty() {
        return text.to_string();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn en(text: &str) -> String {
        remove_discourse_fillers(text, "en")
    }

    #[test]
    fn literal_use_is_never_touched() {
        // The whole reason this is a separate pass, not a filler-list entry.
        assert_eq!(en("do you know the answer"), "do you know the answer");
        assert_eq!(en("I mean it"), "I mean it");
        assert_eq!(en("you know the drill"), "you know the drill");
    }

    #[test]
    fn a_trailing_period_is_not_a_trailing_comma() {
        assert_eq!(
            en("that's how it is, you know."),
            "that's how it is, you know."
        );
        assert_eq!(en("it worked, you know?"), "it worked, you know?");
    }

    #[test]
    fn sentence_initial_marker_capitalizes_what_follows() {
        assert_eq!(
            en("You know, the things are changing"),
            "The things are changing"
        );
        assert_eq!(
            en("I mean, they did some marketing"),
            "They did some marketing"
        );
    }

    #[test]
    fn capitalization_reaches_across_a_full_stop() {
        assert_eq!(
            en("it worked. You know, the rest was easy"),
            "it worked. The rest was easy"
        );
    }

    #[test]
    fn interior_marker_drops_both_commas() {
        assert_eq!(en("boring, you know, movies"), "boring movies");
        assert_eq!(
            en("thinking about, you know, finding a middle ground"),
            "thinking about finding a middle ground"
        );
    }

    #[test]
    fn a_sentence_opener_keeps_its_comma() {
        // "Well," owns its comma; "boring," only had one because of the filler.
        assert_eq!(en("Well, you know, the thing is"), "Well, the thing is");
        assert_eq!(en("Okay, you know, we should go"), "Okay, we should go");
    }

    #[test]
    fn an_opener_mid_sentence_does_not_keep_its_comma() {
        assert_eq!(
            en("we waited and so, you know, we left"),
            "we waited and so we left"
        );
    }

    #[test]
    fn repeated_markers_collapse_in_one_pass() {
        assert_eq!(
            en("not even thinking about, you know, you know, finding a middle ground"),
            "not even thinking about finding a middle ground"
        );
    }

    #[test]
    fn trailing_marker_leaves_no_dangling_comma() {
        assert_eq!(
            en("that is the thing is, you know,"),
            "that is the thing is"
        );
    }

    #[test]
    fn newlines_survive() {
        assert_eq!(
            en("first line, you know, here\nsecond line"),
            "first line here\nsecond line"
        );
    }

    #[test]
    fn a_newline_in_front_of_a_removed_marker_survives() {
        // Regression: the marker's own gap is discarded with it, so a line
        // break sitting immediately before one has to be carried forward or two
        // hard-bounded segments merge into one.
        // A line break is not a full stop, so nothing is re-capitalised here.
        assert_eq!(
            en("first line\nyou know, second line"),
            "first line\nsecond line"
        );
    }

    #[test]
    fn a_leading_newline_is_not_swallowed() {
        assert_eq!(en("\nYou know, hi"), "\nHi");
    }

    #[test]
    fn a_newline_inside_a_marker_survives() {
        // Regression: the break can fall between the marker's own two words,
        // so every gap in the removed span has to be inspected, not just the
        // one preceding it.
        assert_eq!(en("hi you\nknow, there"), "hi\nthere");
    }

    #[test]
    fn an_opener_starting_a_later_sentence_keeps_its_comma() {
        // Regression: "first word of a sentence", not "first word of the
        // transcript". Testing only the text-initial case hid this.
        assert_eq!(
            en("It failed. Well, you know, we tried."),
            "It failed. Well, we tried."
        );
        assert_eq!(
            en("That broke. So, you know, we reverted."),
            "That broke. So, we reverted."
        );
    }

    #[test]
    fn non_english_is_left_alone() {
        let french = "je pense, you know, que oui";
        assert_eq!(remove_discourse_fillers(french, "fr"), french);
        assert_eq!(remove_discourse_fillers(french, "fr-CA"), french);
    }

    #[test]
    fn english_subtags_are_recognized() {
        assert_eq!(
            remove_discourse_fillers("boring, you know, movies", "en-GB"),
            "boring movies"
        );
        assert_eq!(
            remove_discourse_fillers("boring, you know, movies", "en_US"),
            "boring movies"
        );
    }

    #[test]
    fn a_transcript_is_never_emptied() {
        assert_eq!(en("you know,"), "you know,");
        assert_eq!(en("I mean,"), "I mean,");
    }

    #[test]
    fn empty_and_whitespace_are_stable() {
        assert_eq!(en(""), "");
        assert_eq!(en("   "), "   ");
        assert_eq!(en("\n\n"), "\n\n");
    }

    #[test]
    fn running_twice_matches_running_once() {
        let inputs = [
            "boring, you know, movies",
            "Well, you know, the thing is",
            "You know, the things are changing",
            "do you know the answer",
            "not even thinking about, you know, you know, finding",
            "first line, you know, here\nsecond line",
        ];
        for input in inputs {
            let once = en(input);
            let twice = en(&once);
            assert_eq!(once, twice, "not idempotent for {input:?}");
        }
    }

    #[test]
    fn text_without_markers_is_returned_unchanged() {
        let plain = "I checked the positioning and everything.";
        assert_eq!(en(plain), plain);
    }

    #[test]
    fn case_is_matched_insensitively() {
        assert_eq!(en("boring, YOU KNOW, movies"), "boring movies");
        assert_eq!(en("boring, You Know, movies"), "boring movies");
    }

    #[test]
    fn a_split_marker_is_not_matched() {
        // "you, know," is not the phrase; the interior word carries punctuation.
        let split = "do you, know, the answer";
        assert_eq!(en(split), split);
    }

    #[test]
    fn real_transcript_excerpt() {
        // From the repo owner's own history.
        let input = "So essentially, the summary is that I was finally subscribed \
                     to Netflix, and you know, I was very sick of tired and boring, \
                     you know, movies. You know, I gave you detailed prompts.";
        let expected = "So essentially, the summary is that I was finally subscribed \
                        to Netflix, and I was very sick of tired and boring movies. \
                        I gave you detailed prompts.";
        assert_eq!(en(input), expected);
    }
}
