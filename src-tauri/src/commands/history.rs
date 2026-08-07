use crate::actions::process_transcription_output;
use crate::managers::{
    history::{HistoryManager, PaginatedHistory, StatsRange, UsageRange, UsageStats},
    transcription::TranscriptionManager,
};
use std::sync::Arc;
use tauri::{AppHandle, State};

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

#[tauri::command]
#[specta::specta]
pub async fn get_usage_stats(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    range: StatsRange,
) -> Result<UsageStats, String> {
    history_manager
        .get_usage_stats(range)
        .map_err(|e| e.to_string())
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
