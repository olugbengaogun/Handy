use crate::actions::process_transcription_output;
use crate::managers::{
    export::{self, ExportFormat},
    history::{HistoryManager, PaginatedHistory, UsageRange},
    transcription::TranscriptionManager,
};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// What an export actually wrote, so the UI can confirm rather than assume.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct ExportSummary {
    /// Transcripts included.
    pub transcripts: u32,
    /// Phrase corrections included — only ever non-zero for training exports.
    pub corrections: u32,
    /// Transcripts that yielded a messy/clean training pair. Lower than
    /// `transcripts` whenever rows predate `original_text` or were never edited,
    /// which is worth surfacing: a training export can legitimately be empty.
    pub training_pairs: u32,
    pub path: String,
    pub bytes: u64,
}

/// Write selected transcripts to a file the user picked.
///
/// Read-only against the database; the one side effect is the destination file.
/// No audio and no network — this is the whole feature's blast radius.
#[tauri::command]
#[specta::specta]
pub async fn export_history(
    history_manager: State<'_, Arc<HistoryManager>>,
    format: ExportFormat,
    from: Option<i64>,
    to: Option<i64>,
    ids: Option<Vec<i64>>,
    verified_only: bool,
    destination: String,
) -> Result<ExportSummary, String> {
    let rows = history_manager
        .collect_export_rows(from, to, ids.as_deref(), verified_only)
        .map_err(|e| format!("Could not read transcripts: {e}"))?;

    // Only the training format consults corrections, so the query is skipped
    // entirely for the readable ones.
    let corrections = if format == ExportFormat::TrainingJsonl {
        history_manager
            .collect_corrections(from, to)
            .map_err(|e| format!("Could not read corrections: {e}"))?
    } else {
        Vec::new()
    };

    let contents = export::render(format, &rows, &corrections);

    // Count pairs the same way the renderer does, so the summary cannot claim
    // something the file does not contain.
    let training_pairs = rows
        .iter()
        .filter(|row| {
            row.original
                .as_deref()
                .is_some_and(|original| original.trim() != row.text.trim())
        })
        .count() as u32;

    let mut path = std::path::PathBuf::from(&destination);
    // A save dialog does not reliably append its filter's extension when the
    // user types a bare name, and a `.jsonl` export saved as `notes` opens as
    // nothing in particular. Only filled in when absent, so a deliberate
    // extension is always respected.
    if path.extension().is_none() {
        path.set_extension(format.extension());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, contents.as_bytes())
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;

    Ok(ExportSummary {
        transcripts: rows.len() as u32,
        corrections: corrections.len() as u32,
        training_pairs,
        // The path actually written, which may differ from what was requested.
        path: path.to_string_lossy().into_owned(),
        bytes: contents.len() as u64,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn get_history_entries(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<PaginatedHistory, String> {
    history_manager
        .get_history_entries(cursor, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn search_history_entries(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    query: String,
    cursor: Option<i64>,
    limit: Option<usize>,
) -> Result<PaginatedHistory, String> {
    history_manager
        .search_history_entries(&query, cursor, limit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_history_entry_saved(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .toggle_saved_status(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_audio_file_path(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    file_name: String,
) -> Result<String, String> {
    let path = history_manager.get_audio_file_path(&file_name);
    path.to_str()
        .ok_or_else(|| "Invalid file path".to_string())
        .map(|s| s.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn delete_history_entry(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
) -> Result<(), String> {
    history_manager
        .delete_entry(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn retry_history_entry_transcription(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    id: i64,
) -> Result<(), String> {
    let entry = history_manager
        .get_entry_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("History entry {} not found", id))?;

    if !entry.has_audio {
        return Err("Audio for this entry was not kept, so it can't be re-transcribed".to_string());
    }

    let audio_path = history_manager.get_audio_file_path(&entry.file_name);
    let samples = crate::audio_toolkit::read_wav_samples(&audio_path)
        .map_err(|e| format!("Failed to load audio: {}", e))?;

    if samples.is_empty() {
        return Err("Recording has no audio samples".to_string());
    }

    transcription_manager.initiate_model_load();

    let tm = Arc::clone(&transcription_manager);
    let transcription = tauri::async_runtime::spawn_blocking(move || tm.transcribe(samples))
        .await
        .map_err(|e| format!("Transcription task panicked: {}", e))?
        .map_err(|e| e.to_string())?;

    if transcription.is_empty() {
        return Err("Recording contains no speech".to_string());
    }

    let processed =
        process_transcription_output(&app, &transcription, entry.post_process_requested).await;
    history_manager
        .update_transcription(
            id,
            transcription,
            processed.post_processed_text,
            processed.post_process_prompt,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn update_history_limit(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    limit: usize,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.history_limit = limit;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn update_recording_retention_period(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    period: String,
) -> Result<(), String> {
    use crate::settings::RecordingRetentionPeriod;

    let retention_period = match period.as_str() {
        "never" => RecordingRetentionPeriod::Never,
        "preserve_limit" => RecordingRetentionPeriod::PreserveLimit,
        "days3" => RecordingRetentionPeriod::Days3,
        "weeks2" => RecordingRetentionPeriod::Weeks2,
        "months3" => RecordingRetentionPeriod::Months3,
        _ => return Err(format!("Invalid retention period: {}", period)),
    };

    let mut settings = crate::settings::get_settings(&app);
    settings.recording_retention_period = retention_period;
    crate::settings::write_settings(&app, settings);

    history_manager
        .cleanup_old_entries()
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn update_keep_audio_recordings(app: AppHandle, keep: bool) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.keep_audio_recordings = keep;
    crate::settings::write_settings(&app, settings);
    Ok(())
}

/// Per-day dictation totals between two local `YYYY-MM-DD` dates, inclusive,
/// oldest first, with empty days included as zeroes. An absent `start` means
/// "from the first day ever recorded"; an absent `end` means today.
///
/// The single read path behind the Insights panel — tiles, chart and streak
/// grid all derive from this one series.
#[tauri::command]
#[specta::specta]
pub async fn get_usage_range(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    start: Option<String>,
    end: Option<String>,
) -> Result<UsageRange, String> {
    history_manager
        .get_usage_range(start, end)
        .map_err(|e| e.to_string())
}

/// Lets the user manually correct a saved transcript (e.g. fixing a misheard
/// name), independent of retry/re-transcription. Reuses the existing
/// `HistoryManager::update_transcription`, previously only called internally
/// by retry.
#[tauri::command]
#[specta::specta]
pub async fn update_history_transcription(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    text: String,
) -> Result<(), String> {
    let entry = history_manager
        .get_entry_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("History entry {} not found", id))?;

    let original = entry.transcription_text.clone();

    // The derived fields are deliberately cleared rather than carried over.
    // `post_processed_text` was produced by a cleanup model from the *previous*
    // transcript; once the user rewrites that transcript by hand it describes
    // text that no longer exists, and keeping it would leave the row asserting
    // two different things at once. Retry is unaffected — it recomputes both
    // fields and passes the fresh values.
    history_manager
        .update_transcription(id, text.clone(), None, None)
        .map_err(|e| e.to_string())?;

    // A hand-edited transcript is the only reference text in the database a
    // human has actually vouched for. Best-effort: provenance is worth
    // recording, never worth failing the user's edit over.
    if let Err(e) = history_manager.mark_verified(id) {
        log::warn!("Failed to mark history entry {id} as verified: {e}");
    }

    // Learn from the correction. This is the whole point of the learning loop:
    // a hand-edit is the clearest signal the app ever gets about how this
    // particular person speaks, and it costs no audio to capture.
    //
    // Deliberately best-effort and *after* the update has already succeeded —
    // the user's edit is the operation they asked for, and no failure in an
    // optional learning step may be allowed to fail it or lose their text.
    record_correction_for_learning(&_app, id, &original, &text);

    Ok(())
}

/// Record what changed between the stored transcript and the user's edit.
///
/// Errors are logged and swallowed on purpose — see the call site. The manager
/// is constructed on demand rather than held in Tauri state because it owns
/// nothing but a path; the schema itself is created by `HistoryManager`'s
/// migrations, which have already run by the time any command can be invoked.
fn record_correction_for_learning(app: &AppHandle, id: i64, original: &str, edited: &str) {
    if original == edited {
        return;
    }

    let settings = crate::settings::get_settings(app);
    let manager = match crate::managers::learning::LearningManager::new(app) {
        Ok(manager) => manager,
        Err(e) => {
            log::warn!("Learning loop unavailable: {e}");
            return;
        }
    };

    match manager.record_edit(
        Some(id),
        original,
        edited,
        Some(&settings.selected_model),
        Some(&settings.selected_language),
    ) {
        Ok(0) => {}
        Ok(n) => log::debug!("Learning loop captured {n} correction(s) from history edit {id}"),
        Err(e) => log::warn!("Failed to record learning events for entry {id}: {e}"),
    }
}
