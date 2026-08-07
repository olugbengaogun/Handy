use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, Utc};
use log::{debug, error, info};
use rusqlite::{named_params, params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri_specta::Event;

/// Database migrations for transcription history.
/// Each migration is applied in order. The library tracks which migrations
/// have been applied using SQLite's user_version pragma.
///
/// Note: For users upgrading from tauri-plugin-sql, migrate_from_tauri_plugin_sql()
/// converts the old _sqlx_migrations table tracking to the user_version pragma,
/// ensuring migrations don't re-run on existing databases.
static MIGRATIONS: &[M] = &[
    M::up(
        "CREATE TABLE IF NOT EXISTS transcription_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_name TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            saved BOOLEAN NOT NULL DEFAULT 0,
            title TEXT NOT NULL,
            transcription_text TEXT NOT NULL
        );",
    ),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_processed_text TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_prompt TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN post_process_requested BOOLEAN NOT NULL DEFAULT 0;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN has_audio BOOLEAN NOT NULL DEFAULT 1;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN word_count INTEGER NOT NULL DEFAULT 0;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN duration_secs REAL NOT NULL DEFAULT 0;"),
    // Learning loop (see `managers::learning`). Purely additive: a new table
    // plus two nullable columns, so an existing database upgrades in place and
    // every pre-existing query keeps working untouched. The columns record
    // which model and language produced a transcript, without which a learned
    // correction cannot be attributed — a rule taught under one model may be
    // wrong for another.
    M::up(
        "CREATE TABLE IF NOT EXISTS learning_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            history_id INTEGER,
            first_seen INTEGER NOT NULL,
            last_seen INTEGER NOT NULL,
            before_text TEXT NOT NULL,
            after_text TEXT NOT NULL,
            before_key TEXT NOT NULL,
            after_key TEXT NOT NULL,
            edit_kind TEXT NOT NULL,
            model_id TEXT,
            language TEXT,
            occurrences INTEGER NOT NULL DEFAULT 1,
            status TEXT NOT NULL DEFAULT 'pending'
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_learning_events_key
            ON learning_events (before_key, after_key);
        CREATE INDEX IF NOT EXISTS idx_learning_events_status
            ON learning_events (status, occurrences DESC);",
    ),
    M::up("ALTER TABLE transcription_history ADD COLUMN model_id TEXT;"),
    M::up("ALTER TABLE transcription_history ADD COLUMN language TEXT;"),
    // When the user hand-corrects a transcript, the stored text stops being a
    // guess and becomes a human-verified reference. Nothing recorded that
    // before, so `scripts/wer-bench.ts` had to treat *every* stored transcript
    // as ground truth — including the ones nobody ever read. This column is the
    // difference between a benchmark and a number.
    //
    // Deliberately not added to `HistoryEntry`: no Rust type changes means no
    // regenerated `bindings.ts`, and the frontend neither knows nor cares.
    M::up("ALTER TABLE transcription_history ADD COLUMN verified_at INTEGER;"),
    // One-time data fix-ups that need real Rust, not SQL, record themselves
    // here. `learning::rekey_v1` re-normalises correction keys through the same
    // word-core function the matcher uses, which SQLite's string functions
    // cannot express per-word. The migration list can only create the marker;
    // the work itself runs once at startup and writes the row.
    M::up(
        "CREATE TABLE IF NOT EXISTS schema_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    ),
    // Usage stats used to be computed by scanning transcription_history, which
    // made them quietly false. `cleanup_by_count` keeps only the newest
    // `history_limit` unsaved rows, so that table is a rolling window, not a
    // history: past a few days of real use, "this week", "this month" and "all
    // time" all read the same surviving rows and return identical numbers. The
    // Insights panel was not broken - it was faithfully reporting a table that
    // had been deleted underneath it.
    //
    // This aggregate is never pruned. One row per day is ~366 rows a year, so
    // there is nothing to reclaim, and the numbers stop depending on a retention
    // setting that has nothing to do with them.
    //
    // `day` is a LOCAL date, computed once at write time and then frozen. A
    // stored string rather than a derived one: recomputing the bucket later
    // would silently reshuffle history when the user changes timezone, and a
    // streak that moves because someone took a flight is worse than no streak.
    M::up(
        "CREATE TABLE IF NOT EXISTS usage_daily (
            day TEXT PRIMARY KEY,
            words INTEGER NOT NULL DEFAULT 0,
            entries INTEGER NOT NULL DEFAULT 0,
            duration_secs REAL NOT NULL DEFAULT 0,
            corrections INTEGER NOT NULL DEFAULT 0
        );
        -- Backfill from whatever pruning has left, so nobody upgrades into an
        -- empty panel. This can only recover surviving rows; it is the last
        -- moment the old data is worth anything, and from here the aggregate
        -- accumulates properly. SQLite's 'localtime' matches the chrono::Local
        -- bucketing used for new rows.
        INSERT OR IGNORE INTO usage_daily (day, words, entries, duration_secs, corrections)
            SELECT date(timestamp, 'unixepoch', 'localtime'),
                   COALESCE(SUM(word_count), 0),
                   COUNT(*),
                   COALESCE(SUM(duration_secs), 0.0),
                   0
            FROM transcription_history
            WHERE transcription_text != ''
            GROUP BY 1;",
    ),
];

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PaginatedHistory {
    pub entries: Vec<HistoryEntry>,
    pub has_more: bool,
}

/// One local day's dictation totals. Days with no dictation are present with
/// zeroes rather than absent — see [`HistoryManager::get_usage_daily`].
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DailyUsage {
    /// Local calendar day, `YYYY-MM-DD`.
    pub day: String,
    pub words: i64,
    pub entries: i64,
    pub duration_secs: f64,
}

/// A window of daily usage plus the extremes of the whole record.
///
/// The bounds travel with the window because the UI needs both in the same
/// paint: they decide whether the "previous period" arrow is still live, and
/// they anchor an open-ended "all time" start.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct UsageRange {
    pub days: Vec<DailyUsage>,
    /// Earliest local day with any recorded dictation, if there is any.
    pub first_recorded: Option<String>,
    /// Latest local day with any recorded dictation, if there is any.
    pub last_recorded: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StatsRange {
    /// New in this change. Safe to add anywhere: the wire format is
    /// `snake_case` by name (`"today"`), never an ordinal, so the existing
    /// variants keep serialising exactly as before and an older frontend that
    /// never sends "today" is unaffected.
    Today,
    Week,
    Month,
    AllTime,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct UsageStats {
    pub total_words: i64,
    pub total_entries: i64,
    pub total_duration_secs: f64,
    pub average_wpm: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(tag = "action")]
pub enum HistoryUpdatePayload {
    #[serde(rename = "added")]
    Added { entry: HistoryEntry },
    #[serde(rename = "updated")]
    Updated { entry: HistoryEntry },
    #[serde(rename = "deleted")]
    Deleted { id: i64 },
    #[serde(rename = "toggled")]
    Toggled { id: i64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct HistoryEntry {
    pub id: i64,
    pub file_name: String,
    pub timestamp: i64,
    pub saved: bool,
    pub title: String,
    pub transcription_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_prompt: Option<String>,
    pub post_process_requested: bool,
    pub has_audio: bool,
    pub word_count: i64,
    pub duration_secs: f64,
}

pub struct HistoryManager {
    app_handle: AppHandle,
    recordings_dir: PathBuf,
    db_path: PathBuf,
}

impl HistoryManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        // Create recordings directory in app data dir
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let recordings_dir = app_data_dir.join("recordings");
        let db_path = app_data_dir.join("history.db");

        // Ensure recordings directory exists
        if !recordings_dir.exists() {
            fs::create_dir_all(&recordings_dir)?;
            debug!("Created recordings directory: {:?}", recordings_dir);
        }

        let manager = Self {
            app_handle: app_handle.clone(),
            recordings_dir,
            db_path,
        };

        // Initialize database and run migrations synchronously
        manager.init_database()?;

        Ok(manager)
    }

    fn init_database(&self) -> Result<()> {
        info!("Initializing database at {:?}", self.db_path);

        let mut conn = Connection::open(&self.db_path)?;

        // Handle migration from tauri-plugin-sql to rusqlite_migration
        // tauri-plugin-sql used _sqlx_migrations table, rusqlite_migration uses user_version pragma
        self.migrate_from_tauri_plugin_sql(&conn)?;

        // Create migrations object and run to latest version
        let migrations = Migrations::new(MIGRATIONS.to_vec());

        // Validate migrations in debug builds
        #[cfg(debug_assertions)]
        migrations.validate().expect("Invalid migrations");

        // Get current version before migration
        let version_before: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        debug!("Database version before migration: {}", version_before);

        // Apply any pending migrations
        migrations.to_latest(&mut conn)?;

        // Get version after migration
        let version_after: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if version_after > version_before {
            info!(
                "Database migrated from version {} to {}",
                version_before, version_after
            );
        } else {
            debug!("Database already at latest version {}", version_after);
        }

        Ok(())
    }

    /// Migrate from tauri-plugin-sql's migration tracking to rusqlite_migration's.
    /// tauri-plugin-sql used a _sqlx_migrations table, while rusqlite_migration uses
    /// SQLite's user_version pragma. This function checks if the old system was in use
    /// and sets the user_version accordingly so migrations don't re-run.
    fn migrate_from_tauri_plugin_sql(&self, conn: &Connection) -> Result<()> {
        // Check if the old _sqlx_migrations table exists
        let has_sqlx_migrations: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_sqlx_migrations {
            return Ok(());
        }

        // Check current user_version
        let current_version: i32 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version > 0 {
            // Already migrated to rusqlite_migration system
            return Ok(());
        }

        // Get the highest version from the old migrations table
        let old_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if old_version > 0 {
            info!(
                "Migrating from tauri-plugin-sql (version {}) to rusqlite_migration",
                old_version
            );

            // Set user_version to match the old migration state
            conn.pragma_update(None, "user_version", old_version)?;

            // Optionally drop the old migrations table (keeping it doesn't hurt)
            // conn.execute("DROP TABLE IF EXISTS _sqlx_migrations", [])?;

            info!(
                "Migration tracking converted: user_version set to {}",
                old_version
            );
        }

        Ok(())
    }

    fn get_connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    fn map_history_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
        Ok(HistoryEntry {
            id: row.get("id")?,
            file_name: row.get("file_name")?,
            timestamp: row.get("timestamp")?,
            saved: row.get("saved")?,
            title: row.get("title")?,
            transcription_text: row.get("transcription_text")?,
            post_processed_text: row.get("post_processed_text")?,
            post_process_prompt: row.get("post_process_prompt")?,
            post_process_requested: row.get("post_process_requested")?,
            has_audio: row.get("has_audio")?,
            word_count: row.get("word_count")?,
            duration_secs: row.get("duration_secs")?,
        })
    }

    pub fn recordings_dir(&self) -> &std::path::Path {
        &self.recordings_dir
    }

    /// Save a new history entry to the database.
    /// The WAV file should already have been written to the recordings directory
    /// (unless `has_audio` is false, meaning the caller intentionally isn't
    /// keeping it and has already deleted or will delete the WAV).
    pub fn save_entry(
        &self,
        file_name: String,
        transcription_text: String,
        post_process_requested: bool,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
        has_audio: bool,
        duration_secs: f64,
    ) -> Result<HistoryEntry> {
        let timestamp = Utc::now().timestamp();
        let title = self.format_timestamp_title(timestamp);
        let word_count = transcription_text.split_whitespace().count() as i64;

        let mut conn = self.get_connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                has_audio,
                word_count,
                duration_secs
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &file_name,
                timestamp,
                false,
                &title,
                &transcription_text,
                &post_processed_text,
                &post_process_prompt,
                post_process_requested,
                has_audio,
                word_count,
                duration_secs,
            ],
        )?;

        // In the same transaction as the row it summarises, so the aggregate can
        // never drift from the history by a half-applied write.
        //
        // Insert-time only. A later hand-edit recomputes word_count on the
        // history row but deliberately does not touch this: the aggregate
        // records what was dictated that day, and correcting a transcript does
        // not change what was said.
        if !transcription_text.trim().is_empty() {
            Self::record_daily_usage(&tx, timestamp, word_count, duration_secs)?;
        }

        let id = tx.last_insert_rowid();
        tx.commit()?;

        let entry = HistoryEntry {
            id,
            file_name,
            timestamp,
            saved: false,
            title,
            transcription_text,
            post_processed_text,
            post_process_prompt,
            post_process_requested,
            has_audio,
            word_count,
            duration_secs,
        };

        debug!("Saved history entry with id {}", entry.id);

        self.cleanup_old_entries()?;

        // Emit typed event for real-time frontend updates
        if let Err(e) = (HistoryUpdatePayload::Added {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    /// Update an existing history entry with new transcription results (used by retry).
    pub fn update_transcription(
        &self,
        id: i64,
        transcription_text: String,
        post_processed_text: Option<String>,
        post_process_prompt: Option<String>,
    ) -> Result<HistoryEntry> {
        // word_count must be recomputed here (used by retry AND manual edits) —
        // otherwise usage stats keep summing the word count from the original
        // save, silently drifting from the text actually stored.
        let word_count = transcription_text.split_whitespace().count() as i64;

        // Any rewrite of the text invalidates a previous verification: `retry`
        // replaces the transcript with fresh *model output*, and a row still
        // flagged as human-verified would feed that output back to
        // `wer-bench --verified-only` as ground truth — a model grading its own
        // homework. Cleared unconditionally here so the guarantee holds for
        // every caller; the manual-edit path re-asserts it immediately
        // afterwards via `mark_verified`, which is the one caller where a human
        // actually read the result.
        let conn = self.get_connection()?;
        let updated = conn.execute(
            "UPDATE transcription_history
             SET transcription_text = ?1,
                 post_processed_text = ?2,
                 post_process_prompt = ?3,
                 word_count = ?4,
                 verified_at = NULL
             WHERE id = ?5",
            params![
                transcription_text,
                post_processed_text,
                post_process_prompt,
                word_count,
                id
            ],
        )?;

        if updated == 0 {
            return Err(anyhow!("History entry {} not found", id));
        }

        let entry = conn
            .query_row(
                "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, has_audio, word_count, duration_secs
                 FROM transcription_history WHERE id = ?1",
                params![id],
                Self::map_history_entry,
            )?;

        debug!("Updated transcription for history entry {}", id);

        if let Err(e) = (HistoryUpdatePayload::Updated {
            entry: entry.clone(),
        })
        .emit(&self.app_handle)
        {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(entry)
    }

    /// Record that a human has read this transcript and vouched for its text.
    ///
    /// Called after a manual edit in History, which is the one moment the app
    /// can be sure a person compared the transcript against what they meant.
    /// `scripts/wer-bench.ts --verified-only` evaluates against exactly these
    /// rows, so an accuracy number can be trusted rather than merely computed:
    /// without this, an untouched transcript that was simply *wrong* counted as
    /// ground truth and quietly flattered every model.
    ///
    /// Idempotent by intent — a later edit refreshes the timestamp, because the
    /// verification that matters is the most recent one. Best-effort at the call
    /// site: failing to record provenance must never fail the user's edit.
    pub fn mark_verified(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;
        conn.execute(
            "UPDATE transcription_history SET verified_at = ?1 WHERE id = ?2",
            params![Utc::now().timestamp(), id],
        )?;
        Ok(())
    }

    pub fn cleanup_old_entries(&self) -> Result<()> {
        let retention_period = crate::settings::get_recording_retention_period(&self.app_handle);

        match retention_period {
            crate::settings::RecordingRetentionPeriod::Never => {
                // Don't delete anything
                Ok(())
            }
            crate::settings::RecordingRetentionPeriod::PreserveLimit => {
                // Use the old count-based logic with history_limit
                let limit = crate::settings::get_history_limit(&self.app_handle);
                self.cleanup_by_count(limit)
            }
            _ => {
                // Use time-based logic
                self.cleanup_by_time(retention_period)
            }
        }
    }

    fn delete_entries_and_files(&self, entries: &[(i64, String)]) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let conn = self.get_connection()?;
        let mut deleted_count = 0;

        for (id, file_name) in entries {
            // Delete database entry
            conn.execute(
                "DELETE FROM transcription_history WHERE id = ?1",
                params![id],
            )?;

            // Delete WAV file
            let file_path = self.recordings_dir.join(file_name);
            if file_path.exists() {
                if let Err(e) = fs::remove_file(&file_path) {
                    error!("Failed to delete WAV file {}: {}", file_name, e);
                } else {
                    debug!("Deleted old WAV file: {}", file_name);
                    deleted_count += 1;
                }
            }
        }

        Ok(deleted_count)
    }

    fn cleanup_by_count(&self, limit: usize) -> Result<()> {
        let conn = self.get_connection()?;

        // Get all entries that are not saved, ordered by timestamp desc
        let mut stmt = conn.prepare(
            "SELECT id, file_name FROM transcription_history WHERE saved = 0 ORDER BY timestamp DESC"
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
        })?;

        let mut entries: Vec<(i64, String)> = Vec::new();
        for row in rows {
            entries.push(row?);
        }

        if entries.len() > limit {
            let entries_to_delete = &entries[limit..];
            let deleted_count = self.delete_entries_and_files(entries_to_delete)?;

            if deleted_count > 0 {
                debug!("Cleaned up {} old history entries by count", deleted_count);
            }
        }

        Ok(())
    }

    fn cleanup_by_time(
        &self,
        retention_period: crate::settings::RecordingRetentionPeriod,
    ) -> Result<()> {
        let conn = self.get_connection()?;

        // Calculate cutoff timestamp (current time minus retention period)
        let now = Utc::now().timestamp();
        let cutoff_timestamp = match retention_period {
            crate::settings::RecordingRetentionPeriod::Days3 => now - (3 * 24 * 60 * 60), // 3 days in seconds
            crate::settings::RecordingRetentionPeriod::Weeks2 => now - (2 * 7 * 24 * 60 * 60), // 2 weeks in seconds
            crate::settings::RecordingRetentionPeriod::Months3 => now - (3 * 30 * 24 * 60 * 60), // 3 months in seconds (approximate)
            _ => unreachable!("Should not reach here"),
        };

        // Get all unsaved entries older than the cutoff timestamp
        let mut stmt = conn.prepare(
            "SELECT id, file_name FROM transcription_history WHERE saved = 0 AND timestamp < ?1",
        )?;

        let rows = stmt.query_map(params![cutoff_timestamp], |row| {
            Ok((row.get::<_, i64>("id")?, row.get::<_, String>("file_name")?))
        })?;

        let mut entries_to_delete: Vec<(i64, String)> = Vec::new();
        for row in rows {
            entries_to_delete.push(row?);
        }

        let deleted_count = self.delete_entries_and_files(&entries_to_delete)?;

        if deleted_count > 0 {
            debug!(
                "Cleaned up {} old history entries based on retention period",
                deleted_count
            );
        }

        Ok(())
    }

    pub async fn get_history_entries(
        &self,
        cursor: Option<i64>,
        limit: Option<usize>,
    ) -> Result<PaginatedHistory> {
        let conn = self.get_connection()?;
        let limit = limit.map(|l| l.min(100));

        let mut entries: Vec<HistoryEntry> = match (cursor, limit) {
            (Some(cursor_id), Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, has_audio, word_count, duration_secs
                     FROM transcription_history
                     WHERE id < ?1
                     ORDER BY id DESC
                     LIMIT ?2",
                )?;
                let result = stmt
                    .query_map(params![cursor_id, fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (None, Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, has_audio, word_count, duration_secs
                     FROM transcription_history
                     ORDER BY id DESC
                     LIMIT ?1",
                )?;
                let result = stmt
                    .query_map(params![fetch_count], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (_, None) => {
                let mut stmt = conn.prepare(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, has_audio, word_count, duration_secs
                     FROM transcription_history
                     ORDER BY id DESC",
                )?;
                let result = stmt
                    .query_map([], Self::map_history_entry)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
        };

        let has_more = limit.is_some_and(|lim| entries.len() > lim);
        if has_more {
            entries.pop();
        }

        Ok(PaginatedHistory { entries, has_more })
    }

    /// Same cursor-based pagination as `get_history_entries`, filtered to
    /// entries whose raw or post-processed text contains `query`
    /// (case-insensitive for ASCII, matching SQLite's default LIKE).
    pub async fn search_history_entries(
        &self,
        query: &str,
        cursor: Option<i64>,
        limit: Option<usize>,
    ) -> Result<PaginatedHistory> {
        let conn = self.get_connection()?;
        let limit = limit.map(|l| l.min(100));
        // Escape LIKE's special characters so a literal search for e.g. "50%"
        // or "file_name" isn't misinterpreted as a wildcard pattern.
        let pattern = format!(
            "%{}%",
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        const MATCH_CLAUSE: &str =
            "(transcription_text LIKE :pattern ESCAPE '\\' OR post_processed_text LIKE :pattern ESCAPE '\\')";

        let mut entries: Vec<HistoryEntry> = match (cursor, limit) {
            (Some(cursor_id), Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let sql = format!(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, has_audio, word_count, duration_secs
                     FROM transcription_history
                     WHERE id < :cursor AND {MATCH_CLAUSE}
                     ORDER BY id DESC
                     LIMIT :fetch_count"
                );
                let mut stmt = conn.prepare(&sql)?;
                let result = stmt
                    .query_map(
                        named_params! {
                            ":cursor": cursor_id,
                            ":pattern": pattern,
                            ":fetch_count": fetch_count,
                        },
                        Self::map_history_entry,
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (None, Some(lim)) => {
                let fetch_count = (lim + 1) as i64;
                let sql = format!(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, has_audio, word_count, duration_secs
                     FROM transcription_history
                     WHERE {MATCH_CLAUSE}
                     ORDER BY id DESC
                     LIMIT :fetch_count"
                );
                let mut stmt = conn.prepare(&sql)?;
                let result = stmt
                    .query_map(
                        named_params! {
                            ":pattern": pattern,
                            ":fetch_count": fetch_count,
                        },
                        Self::map_history_entry,
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
            (_, None) => {
                let sql = format!(
                    "SELECT id, file_name, timestamp, saved, title, transcription_text, post_processed_text, post_process_prompt, post_process_requested, has_audio, word_count, duration_secs
                     FROM transcription_history
                     WHERE {MATCH_CLAUSE}
                     ORDER BY id DESC"
                );
                let mut stmt = conn.prepare(&sql)?;
                let result = stmt
                    .query_map(
                        named_params! { ":pattern": pattern },
                        Self::map_history_entry,
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result
            }
        };

        let has_more = limit.is_some_and(|lim| entries.len() > lim);
        if has_more {
            entries.pop();
        }

        Ok(PaginatedHistory { entries, has_more })
    }

    #[cfg(test)]
    fn get_latest_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                has_audio,
                word_count,
                duration_secs
             FROM transcription_history
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    /// Add one transcription to its local day's running totals.
    ///
    /// The day is derived from the entry's own timestamp rather than "now", so a
    /// recording that finishes transcribing just after midnight still counts
    /// toward the day it was spoken.
    fn record_daily_usage(
        conn: &Connection,
        timestamp: i64,
        word_count: i64,
        duration_secs: f64,
    ) -> Result<()> {
        let day = DateTime::from_timestamp(timestamp, 0)
            .ok_or_else(|| anyhow!("timestamp {timestamp} is out of range"))?
            .with_timezone(&Local)
            .format("%Y-%m-%d")
            .to_string();

        conn.execute(
            "INSERT INTO usage_daily (day, words, entries, duration_secs, corrections)
             VALUES (?1, ?2, 1, ?3, 0)
             ON CONFLICT (day) DO UPDATE SET
                words = words + excluded.words,
                entries = entries + 1,
                duration_secs = duration_secs + excluded.duration_secs",
            params![day, word_count, duration_secs],
        )?;
        Ok(())
    }

    /// Per-day totals between two local dates, inclusive, oldest first, with
    /// days the user did not dictate returned as explicit zeroes.
    ///
    /// The gaps matter: a bar chart and a streak calendar are both *about* the
    /// empty days, and a caller left to infer them from missing keys will get
    /// the arithmetic wrong at a month boundary. Filling them here means one
    /// implementation of "what is a day" instead of one per consumer.
    ///
    /// This is deliberately the *only* read path for the Insights panel. The
    /// tiles sum this same series rather than running their own aggregate
    /// query, so a headline figure and the chart under it cannot disagree about
    /// what the selected period contains.
    pub fn get_usage_range(
        &self,
        start: Option<String>,
        end: Option<String>,
    ) -> Result<UsageRange> {
        let conn = self.get_connection()?;
        Self::usage_range_with(&conn, start, end)
    }

    /// The body of [`Self::get_usage_range`], against a caller-supplied
    /// connection so the calendar arithmetic can be tested without a Tauri
    /// AppHandle or a file on disk.
    fn usage_range_with(
        conn: &Connection,
        start: Option<String>,
        end: Option<String>,
    ) -> Result<UsageRange> {
        // The extremes of the whole record, which the UI needs regardless of the
        // window asked for: they decide how far back navigation may go, and
        // they resolve an open-ended "all time" start.
        let (first_recorded, last_recorded): (Option<String>, Option<String>) = conn.query_row(
            "SELECT MIN(day), MAX(day) FROM usage_daily",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let today = Local::now().date_naive();
        let today_str = today.format("%Y-%m-%d").to_string();

        // An absent start means "from the first day ever recorded", never a
        // 1970 epoch: this function gap-fills, so an open start would
        // materialise twenty thousand empty days to render a chart of four.
        let start = start
            .or_else(|| first_recorded.clone())
            .unwrap_or_else(|| today_str.clone());
        let end = end.unwrap_or_else(|| today_str.clone());

        let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d");
        let start_date = parse(&start).map_err(|e| anyhow!("bad start day {start}: {e}"))?;
        let end_date = parse(&end).map_err(|e| anyhow!("bad end day {end}: {e}"))?;

        // An inverted range is a caller bug, not a reason to fail: return an
        // empty series so the panel renders its empty state instead of an error
        // toast the user can do nothing about.
        if end_date < start_date {
            return Ok(UsageRange {
                days: Vec::new(),
                first_recorded,
                last_recorded,
            });
        }

        // Hard ceiling on materialised rows. Ten years of daily squares is far
        // past anything the UI draws, and it bounds the allocation whatever the
        // caller asks for.
        const MAX_DAYS: i64 = 3700;
        let span = (end_date - start_date).num_days();
        let start_date = if span >= MAX_DAYS {
            end_date - chrono::Duration::days(MAX_DAYS - 1)
        } else {
            start_date
        };
        let start = start_date.format("%Y-%m-%d").to_string();

        let mut stmt = conn.prepare(
            "SELECT day, words, entries, duration_secs
             FROM usage_daily WHERE day >= ?1 AND day <= ?2",
        )?;
        let rows = stmt.query_map(params![start, end], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })?;

        let mut found = std::collections::HashMap::new();
        for row in rows {
            let (day, words, entries, duration_secs) = row?;
            found.insert(day, (words, entries, duration_secs));
        }

        let mut days = Vec::new();
        let mut cursor = start_date;
        while cursor <= end_date {
            let day = cursor.format("%Y-%m-%d").to_string();
            let (words, entries, duration_secs) = found.get(&day).copied().unwrap_or((0, 0, 0.0));
            days.push(DailyUsage {
                day,
                words,
                entries,
                duration_secs,
            });
            cursor += chrono::Duration::days(1);
        }

        Ok(UsageRange {
            days,
            first_recorded,
            last_recorded,
        })
    }

    /// The local date `days_ago` days before today, as the `YYYY-MM-DD` string
    /// used by `usage_daily.day`.
    ///
    /// Calendar arithmetic on local dates, not `now - N * 86400`: subtracting
    /// seconds crosses a DST boundary an hour early or late, which on the wrong
    /// day silently includes or drops a whole day of usage.
    fn local_day_offset(days_ago: i64) -> String {
        (Local::now().date_naive() - chrono::Duration::days(days_ago))
            .format("%Y-%m-%d")
            .to_string()
    }

    /// Aggregate word/duration/entry-count stats over a time range.
    ///
    /// Reads `usage_daily`, not `transcription_history`. The history table is
    /// pruned to `history_limit` rows, so summing it made every range converge
    /// on the same answer once a user dictated more than that window held - see
    /// the `usage_daily` migration.
    pub fn get_usage_stats(&self, range: StatsRange) -> Result<UsageStats> {
        let conn = self.get_connection()?;
        // Inclusive lower bound on the local day, or None for all time.
        let cutoff = match range {
            StatsRange::Today => Some(Self::local_day_offset(0)),
            StatsRange::Week => Some(Self::local_day_offset(6)),
            StatsRange::Month => Some(Self::local_day_offset(29)),
            StatsRange::AllTime => None,
        };

        let query = "SELECT COALESCE(SUM(words), 0), COALESCE(SUM(entries), 0),
                            COALESCE(SUM(duration_secs), 0.0)
                     FROM usage_daily
                     WHERE (?1 IS NULL OR day >= ?1)";

        let (total_words, total_entries, total_duration_secs): (i64, i64, f64) =
            conn.query_row(query, params![cutoff], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;

        let average_wpm = if total_duration_secs > 0.0 {
            total_words as f64 / (total_duration_secs / 60.0)
        } else {
            0.0
        };

        Ok(UsageStats {
            total_words,
            total_entries,
            total_duration_secs,
            average_wpm,
        })
    }

    /// Get the latest entry with non-empty transcription text.
    pub fn get_latest_completed_entry(&self) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        Self::get_latest_completed_entry_with_conn(&conn)
    }

    fn get_latest_completed_entry_with_conn(conn: &Connection) -> Result<Option<HistoryEntry>> {
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                has_audio,
                word_count,
                duration_secs
             FROM transcription_history
             WHERE transcription_text != ''
             ORDER BY timestamp DESC
             LIMIT 1",
        )?;

        let entry = stmt.query_row([], Self::map_history_entry).optional()?;
        Ok(entry)
    }

    pub async fn toggle_saved_status(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;

        // Get current saved status
        let current_saved: bool = conn.query_row(
            "SELECT saved FROM transcription_history WHERE id = ?1",
            params![id],
            |row| row.get("saved"),
        )?;

        let new_saved = !current_saved;

        conn.execute(
            "UPDATE transcription_history SET saved = ?1 WHERE id = ?2",
            params![new_saved, id],
        )?;

        debug!("Toggled saved status for entry {}: {}", id, new_saved);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Toggled { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    pub fn get_audio_file_path(&self, file_name: &str) -> PathBuf {
        self.recordings_dir.join(file_name)
    }

    pub async fn get_entry_by_id(&self, id: i64) -> Result<Option<HistoryEntry>> {
        let conn = self.get_connection()?;
        let mut stmt = conn.prepare(
            "SELECT
                id,
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested,
                has_audio,
                word_count,
                duration_secs
             FROM transcription_history
             WHERE id = ?1",
        )?;

        let entry = stmt.query_row([id], Self::map_history_entry).optional()?;

        Ok(entry)
    }

    pub async fn delete_entry(&self, id: i64) -> Result<()> {
        let conn = self.get_connection()?;

        // Get the entry to find the file name
        if let Some(entry) = self.get_entry_by_id(id).await? {
            // Delete the audio file first
            let file_path = self.get_audio_file_path(&entry.file_name);
            if file_path.exists() {
                if let Err(e) = fs::remove_file(&file_path) {
                    error!("Failed to delete audio file {}: {}", entry.file_name, e);
                    // Continue with database deletion even if file deletion fails
                }
            }
        }

        // Delete from database
        conn.execute(
            "DELETE FROM transcription_history WHERE id = ?1",
            params![id],
        )?;

        debug!("Deleted history entry with id: {}", id);

        // Emit history updated event
        if let Err(e) = (HistoryUpdatePayload::Deleted { id }).emit(&self.app_handle) {
            error!("Failed to emit history-updated event: {}", e);
        }

        Ok(())
    }

    fn format_timestamp_title(&self, timestamp: i64) -> String {
        if let Some(utc_datetime) = DateTime::from_timestamp(timestamp, 0) {
            // Convert UTC to local timezone
            let local_datetime = utc_datetime.with_timezone(&Local);
            local_datetime.format("%B %e, %Y - %l:%M%p").to_string()
        } else {
            format!("Recording {}", timestamp)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE transcription_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_name TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                saved BOOLEAN NOT NULL DEFAULT 0,
                title TEXT NOT NULL,
                transcription_text TEXT NOT NULL,
                post_processed_text TEXT,
                post_process_prompt TEXT,
                post_process_requested BOOLEAN NOT NULL DEFAULT 0,
                has_audio BOOLEAN NOT NULL DEFAULT 1,
                word_count INTEGER NOT NULL DEFAULT 0,
                duration_secs REAL NOT NULL DEFAULT 0,
                verified_at INTEGER
            );",
        )
        .expect("create transcription_history table");
        conn
    }

    fn usage_conn() -> Connection {
        let conn = setup_conn();
        conn.execute_batch(
            "CREATE TABLE usage_daily (
                day TEXT PRIMARY KEY,
                words INTEGER NOT NULL DEFAULT 0,
                entries INTEGER NOT NULL DEFAULT 0,
                duration_secs REAL NOT NULL DEFAULT 0,
                corrections INTEGER NOT NULL DEFAULT 0
            );",
        )
        .expect("create usage_daily");
        conn
    }

    /// A local-midnight timestamp `days_ago` days back, so a row lands in the
    /// day bucket the test means regardless of the machine's timezone.
    fn ts_days_ago(days_ago: i64) -> i64 {
        (Local::now().date_naive() - chrono::Duration::days(days_ago))
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .unwrap()
            .timestamp()
    }

    fn fill_days(conn: &Connection, start: &str, end: &str) -> Vec<DailyUsage> {
        HistoryManager::usage_range_with(
            conn,
            Some(start.to_string()),
            Some(end.to_string()),
        )
        .expect("usage range")
        .days
    }

    fn sum_for(conn: &Connection, cutoff: Option<String>) -> (i64, i64) {
        conn.query_row(
            "SELECT COALESCE(SUM(words), 0), COALESCE(SUM(entries), 0)
             FROM usage_daily WHERE (?1 IS NULL OR day >= ?1)",
            params![cutoff],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("sum usage_daily")
    }

    #[test]
    fn daily_usage_buckets_by_local_day_and_accumulates() {
        let conn = usage_conn();
        HistoryManager::record_daily_usage(&conn, ts_days_ago(0), 100, 60.0).unwrap();
        HistoryManager::record_daily_usage(&conn, ts_days_ago(0), 50, 30.0).unwrap();
        HistoryManager::record_daily_usage(&conn, ts_days_ago(10), 7, 5.0).unwrap();

        let days: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(days, 2, "two distinct local days");

        let (words, entries) = sum_for(&conn, Some(HistoryManager::local_day_offset(0)));
        assert_eq!((words, entries), (150, 2), "today's two entries accumulate");
    }

    /// The bug this whole table exists to fix: every range used to return the
    /// same numbers, because the table being summed was pruned underneath it.
    #[test]
    fn ranges_differ_and_survive_history_pruning() {
        let conn = usage_conn();
        HistoryManager::record_daily_usage(&conn, ts_days_ago(0), 10, 6.0).unwrap();
        HistoryManager::record_daily_usage(&conn, ts_days_ago(3), 20, 12.0).unwrap();
        HistoryManager::record_daily_usage(&conn, ts_days_ago(20), 40, 24.0).unwrap();
        HistoryManager::record_daily_usage(&conn, ts_days_ago(200), 80, 48.0).unwrap();

        let today = sum_for(&conn, Some(HistoryManager::local_day_offset(0))).0;
        let week = sum_for(&conn, Some(HistoryManager::local_day_offset(6))).0;
        let month = sum_for(&conn, Some(HistoryManager::local_day_offset(29))).0;
        let all = sum_for(&conn, None).0;

        assert_eq!((today, week, month, all), (10, 30, 70, 150));
        assert!(
            today < week && week < month && month < all,
            "each range must be a strict superset of the last"
        );

        // Pruning the history must not move any of these numbers - that
        // dependency is exactly what made "all time" a falsehood.
        conn.execute("DELETE FROM transcription_history", [])
            .unwrap();
        assert_eq!(sum_for(&conn, None).0, 150);
    }

    /// Gap-filling has to survive the irregular bit of the calendar: a month
    /// boundary. Feb 27 → Mar 2 is four days in 2026, and any implementation
    /// that reasons in fixed-length months gets it wrong.
    #[test]
    fn range_gap_fills_across_a_month_boundary() {
        let conn = usage_conn();
        let days = fill_days(&conn, "2026-02-27", "2026-03-02");
        assert_eq!(
            days.iter().map(|d| d.day.as_str()).collect::<Vec<_>>(),
            ["2026-02-27", "2026-02-28", "2026-03-01", "2026-03-02"]
        );
        assert!(days.iter().all(|d| d.entries == 0), "absent days read zero");
    }

    #[test]
    fn range_of_one_day_returns_one_day() {
        let conn = usage_conn();
        let days = fill_days(&conn, "2026-08-07", "2026-08-07");
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].day, "2026-08-07");
    }

    /// An inverted range is a caller bug. It must render an empty panel, not
    /// raise an error the user can do nothing about.
    #[test]
    fn inverted_range_is_empty_not_an_error() {
        let conn = usage_conn();
        assert!(fill_days(&conn, "2026-08-07", "2026-08-01").is_empty());
    }

    /// "All time" sends no start. It must resolve to the first *recorded* day —
    /// resolving it to the epoch would gap-fill twenty thousand empty rows to
    /// draw a chart of three.
    #[test]
    fn open_start_resolves_to_the_first_recorded_day() {
        let conn = usage_conn();
        conn.execute(
            "INSERT INTO usage_daily (day, words, entries, duration_secs)
             VALUES ('2026-08-01', 10, 1, 6.0), ('2026-08-04', 20, 2, 12.0)",
            [],
        )
        .unwrap();

        let range = HistoryManager::usage_range_with(
            &conn,
            None,
            Some("2026-08-05".to_string()),
        )
        .expect("usage range");

        assert_eq!(range.first_recorded.as_deref(), Some("2026-08-01"));
        assert_eq!(range.last_recorded.as_deref(), Some("2026-08-04"));
        assert_eq!(range.days.len(), 5, "Aug 1..5 inclusive, gaps filled");
        assert_eq!(range.days[0].day, "2026-08-01");
        assert_eq!(range.days[1].words, 0, "Aug 2 was quiet");
        assert_eq!(range.days[3].words, 20);
    }

    /// A database with no usage at all must not panic or invent a range.
    #[test]
    fn empty_database_reports_no_bounds() {
        let conn = usage_conn();
        let range = HistoryManager::usage_range_with(&conn, None, None).expect("usage range");
        assert!(range.first_recorded.is_none());
        assert!(range.last_recorded.is_none());
        assert_eq!(range.days.len(), 1, "just today, empty");
        assert_eq!(range.days[0].entries, 0);
    }

    /// Guards the DST trap: 30 calendar days back is not 30 * 86400 seconds.
    #[test]
    fn day_offsets_are_calendar_days() {
        let today = HistoryManager::local_day_offset(0);
        let month = HistoryManager::local_day_offset(29);
        assert!(month < today);

        let expected = (Local::now().date_naive() - chrono::Duration::days(29))
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(month, expected);
    }

    fn insert_entry(conn: &Connection, timestamp: i64, text: &str, post_processed: Option<&str>) {
        conn.execute(
            "INSERT INTO transcription_history (
                file_name,
                timestamp,
                saved,
                title,
                transcription_text,
                post_processed_text,
                post_process_prompt,
                post_process_requested
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                format!("handy-{}.wav", timestamp),
                timestamp,
                false,
                format!("Recording {}", timestamp),
                text,
                post_processed,
                Option::<String>::None,
                false,
            ],
        )
        .expect("insert history entry");
    }

    #[test]
    fn get_latest_entry_returns_none_when_empty() {
        let conn = setup_conn();
        let entry = HistoryManager::get_latest_entry_with_conn(&conn).expect("fetch latest entry");
        assert!(entry.is_none());
    }

    #[test]
    fn get_latest_entry_returns_newest_entry() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "first", None);
        insert_entry(&conn, 200, "second", Some("processed"));

        let entry = HistoryManager::get_latest_entry_with_conn(&conn)
            .expect("fetch latest entry")
            .expect("entry exists");

        assert_eq!(entry.timestamp, 200);
        assert_eq!(entry.transcription_text, "second");
        assert_eq!(entry.post_processed_text.as_deref(), Some("processed"));
    }

    #[test]
    fn get_latest_completed_entry_skips_empty_entries() {
        let conn = setup_conn();
        insert_entry(&conn, 100, "completed", None);
        insert_entry(&conn, 200, "", None);

        let entry = HistoryManager::get_latest_completed_entry_with_conn(&conn)
            .expect("fetch latest completed entry")
            .expect("completed entry exists");

        assert_eq!(entry.timestamp, 100);
        assert_eq!(entry.transcription_text, "completed");
    }
}
