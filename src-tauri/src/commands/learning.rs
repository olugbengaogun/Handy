//! Frontend surface for the learning loop.
//!
//! Recording happens automatically whenever the user edits a transcript (see
//! `commands::history::update_history_transcription`). These commands are the
//! read/act side: what has been learned, and what the user wants done with it.
//!
//! Nothing here promotes a correction on its own. A suggestion becomes a rule
//! only when the user says so — the frequency gate decides what is worth
//! *asking* about, never what is worth applying.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;

use crate::audio_toolkit::mine_candidates;
use crate::managers::learning::{status, LearningManager, LearningSuggestion};
use crate::settings::{write_settings, CorrectionPair};

/// How many recent transcripts to mine. Enough to see a term recur, small enough
/// that the scan stays instant on a history that has grown for years.
const TRANSCRIPT_MINING_LIMIT: i64 = 500;

/// Cap on mined terms offered at once, so the review queue cannot be flooded.
/// Attention is the scarce resource here, not candidates.
const MAX_TERM_SUGGESTIONS: usize = 25;

/// A term mined from past transcripts, offered for the dictionary.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct TermSuggestion {
    pub term: String,
    pub occurrences: i64,
}

/// Does this dictionary pair correspond to the given learned correction?
///
/// Uses full Unicode lowercasing rather than `eq_ignore_ascii_case`, which only
/// folds A–Z: a rule taught as "café" → "CAFÉ" would otherwise fail to match
/// its own dictionary entry, so the rule would apply to transcriptions while
/// showing as absent in the UI — and `demote` would refuse to remove it.
fn pair_matches(pair: &CorrectionPair, before: &str, after: &str) -> bool {
    pair.wrong.to_lowercase() == before.to_lowercase()
        && pair.correct.to_lowercase() == after.to_lowercase()
}

/// Corrections the user has made often enough to be worth offering as rules.
#[tauri::command]
#[specta::specta]
pub async fn get_learning_suggestions(
    app: AppHandle,
    min_occurrences: Option<i64>,
) -> Result<Vec<LearningSuggestion>, String> {
    let threshold =
        min_occurrences.unwrap_or(crate::managers::learning::DEFAULT_PROMOTION_THRESHOLD);
    let manager = LearningManager::new(&app).map_err(|e| e.to_string())?;

    // Self-heal before reading. A learned correction whose dictionary entry was
    // deleted elsewhere must become suggestable again, otherwise the loop goes
    // permanently deaf to that one mistake. Cheap and idempotent.
    let settings = crate::settings::get_settings(&app);
    let pairs: Vec<(String, String)> = settings
        .correction_pairs
        .iter()
        .map(|pair| (pair.wrong.clone(), pair.correct.clone()))
        .collect();
    if let Err(e) = manager.reconcile_active(&pairs) {
        // Never fail the read over a repair: a stale row is far better than an
        // empty screen.
        log::warn!("Could not reconcile learned corrections: {e}");
    }

    manager
        .pending_suggestions(threshold)
        .map_err(|e| e.to_string())
}

/// Corrections the user has already promoted into the dictionary.
///
/// Cross-checked against the dictionary itself rather than trusted from the
/// learning table alone: a pair can also be deleted directly from the Advanced
/// settings, and listing a "learned rule" that no longer exists would be a lie
/// the user cannot act on.
#[tauri::command]
#[specta::specta]
pub async fn get_learned_rules(app: AppHandle) -> Result<Vec<LearningSuggestion>, String> {
    let settings = crate::settings::get_settings(&app);
    let rules = LearningManager::new(&app)
        .map_err(|e| e.to_string())?
        .active_rules()
        .map_err(|e| e.to_string())?;

    Ok(rules
        .into_iter()
        .filter(|rule| {
            settings
                .correction_pairs
                .iter()
                .any(|pair| pair_matches(pair, &rule.before, &rule.after))
        })
        .collect())
}

/// Undo a promotion: drop the pair from the dictionary and return the
/// suggestion to `pending`.
///
/// Returned to pending rather than dismissed on purpose. Removing a rule says
/// "this is not right *yet*", not "never ask me again" — if the same correction
/// keeps recurring, the user should get the chance to reconsider. Dismissing is
/// the separate, explicit way to silence something for good.
#[tauri::command]
#[specta::specta]
pub async fn demote_learning_suggestion(app: AppHandle, id: i64) -> Result<(), String> {
    let manager = LearningManager::new(&app).map_err(|e| e.to_string())?;

    let rule = manager
        .active_rules()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("Learned rule {id} not found"))?;

    let mut settings = crate::settings::get_settings(&app);
    let before = settings.correction_pairs.len();
    settings
        .correction_pairs
        .retain(|pair| !pair_matches(pair, &rule.before, &rule.after));

    if settings.correction_pairs.len() != before {
        write_settings(&app, settings);
    }

    manager
        .set_status(id, status::PENDING)
        .map_err(|e| e.to_string())
}

/// Accept a suggestion: add it to `correction_pairs` and mark it active.
///
/// The settings write happens **before** the status flip. If the order were
/// reversed and the write failed, the suggestion would be marked active while no
/// rule existed — it would never be offered again and never do anything, which
/// is the one outcome the user could not recover from through the UI.
#[tauri::command]
#[specta::specta]
pub async fn promote_learning_suggestion(app: AppHandle, id: i64) -> Result<(), String> {
    let manager = LearningManager::new(&app).map_err(|e| e.to_string())?;

    let suggestion = manager
        .pending_suggestions(1)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("Learning suggestion {id} not found or already resolved"))?;

    let mut settings = crate::settings::get_settings(&app);

    // Refuse to promote the inverse of a rule that is already active.
    //
    // This is the loop feeding on its own output. A rule `A → B` rewrites future
    // transcripts; if the user then corrects one of those back to `A`, the
    // learning pass records `B → A` in good faith. Promoting it would leave two
    // rules fighting over the same text, and which one wins depends on nothing
    // more principled than dictionary order. Worse, the pair is self-sustaining:
    // whichever fires produces the input that reinforces the other.
    //
    // A system that learns from its own corrections has to be able to notice
    // when it has started arguing with itself. Surfacing it as an error the user
    // resolves — by removing the rule they no longer want — keeps the
    // contradiction visible instead of silently arbitrary.
    if let Some(conflict) = settings
        .correction_pairs
        .iter()
        .find(|pair| pair_matches(pair, &suggestion.after, &suggestion.before))
    {
        return Err(format!(
            "This is the reverse of a correction you already have \
             (\"{}\" → \"{}\"). Remove that one first, otherwise the two rules \
             would undo each other.",
            conflict.wrong, conflict.correct
        ));
    }

    // Adding a duplicate would make the dictionary grow without bound while the
    // fuzzy matcher pays for every extra entry on every transcription.
    let already_present = settings
        .correction_pairs
        .iter()
        .any(|pair| pair_matches(pair, &suggestion.before, &suggestion.after));

    if !already_present {
        settings.correction_pairs.push(CorrectionPair {
            wrong: suggestion.before.clone(),
            correct: suggestion.after.clone(),
        });
        write_settings(&app, settings);
    }

    manager
        .set_status(id, status::ACTIVE)
        .map_err(|e| e.to_string())
}

/// Recurring proper nouns mined from the user's own past transcripts.
///
/// Solves cold start: the reactive loop needs a correction to happen twice
/// before it can offer anything, but the terms most worth knowing are wrong from
/// the very first dictation. These are only ever *suggestions* — they come from
/// model output, so a consistently mis-heard name is mined in its mis-heard
/// form, and adding one automatically would teach the app to reinforce its own
/// mistake.
#[tauri::command]
#[specta::specta]
pub async fn get_dictionary_candidates(app: AppHandle) -> Result<Vec<TermSuggestion>, String> {
    let settings = crate::settings::get_settings(&app);
    let transcripts = LearningManager::new(&app)
        .map_err(|e| e.to_string())?
        .recent_transcripts(TRANSCRIPT_MINING_LIMIT)
        .map_err(|e| e.to_string())?;

    // Exclude anything already known, whether it came from the plain word list
    // or from a taught correction.
    let known = settings.effective_custom_words();

    Ok(mine_candidates(&transcripts, &known)
        .into_iter()
        .take(MAX_TERM_SUGGESTIONS)
        .map(|c| TermSuggestion {
            term: c.term,
            occurrences: c.occurrences as i64,
        })
        .collect())
}

/// Add a mined term to the custom-word dictionary.
#[tauri::command]
#[specta::specta]
pub async fn accept_dictionary_candidate(app: AppHandle, term: String) -> Result<(), String> {
    let term = term.trim().to_string();
    if term.is_empty() {
        return Err("Cannot add an empty term".to_string());
    }

    let mut settings = crate::settings::get_settings(&app);
    if settings
        .effective_custom_words()
        .iter()
        .any(|existing| existing.to_lowercase() == term.to_lowercase())
    {
        // Already covered; treat as success so a double-click is harmless.
        return Ok(());
    }

    settings.custom_words.push(term);
    write_settings(&app, settings);
    Ok(())
}

/// Refuse a suggestion. It keeps accumulating occurrences so the history stays
/// honest, but it is never offered again — re-asking about something the user
/// already declined is how a helpful feature becomes a nag.
#[tauri::command]
#[specta::specta]
pub async fn dismiss_learning_suggestion(app: AppHandle, id: i64) -> Result<(), String> {
    LearningManager::new(&app)
        .map_err(|e| e.to_string())?
        .set_status(id, status::DISMISSED)
        .map_err(|e| e.to_string())
}
