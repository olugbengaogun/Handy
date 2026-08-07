//! The learning loop: turning the user's hand-edits into correction rules.
//!
//! Every time someone fixes a transcript, the difference between what the model
//! produced and what they meant is recorded here. Once the *same* correction has
//! been made enough times, it is offered for promotion into the correction
//! dictionary, where [`crate::audio_toolkit::corrections`] applies it
//! automatically from then on.
//!
//! ## Why a frequency gate
//!
//! The single most dangerous failure mode in a system that learns from its user
//! is learning the wrong thing and then applying it forever. Three things can
//! produce a bad rule from a good user:
//!
//! 1. A typo in the edit itself.
//! 2. An edit that was not a correction at all — the user changed their mind
//!    about phrasing. Promoting that actively corrupts future transcripts where
//!    the original wording was right.
//! 3. A one-off proper noun that will never recur.
//!
//! Requiring a correction to be made **more than once** filters all three
//! cheaply, because none of them tend to repeat identically. Combined with the
//! type filter in [`EditKind::is_promotable`] — rewrites, insertions and
//! deletions can never be promoted at all — this is what makes automatic
//! learning safe enough to enable by default.
//!
//! Nothing here ever writes to the correction dictionary on its own. It records,
//! counts, and *suggests*; promotion stays an explicit act.

use anyhow::Result;
use log::debug;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;
use tauri::AppHandle;

use crate::audio_toolkit::diff::{core, diff_transcripts, EditKind};

/// How many times a correction must be seen before it is offered for promotion.
///
/// Two is deliberate. One would promote typos and changes of mind; three or more
/// would make the feature feel dead for corrections the user only needs
/// occasionally.
pub const DEFAULT_PROMOTION_THRESHOLD: i64 = 2;

/// Upper bound on suggestions returned in one call, so a pathological history
/// cannot flood the UI.
const MAX_SUGGESTIONS: i64 = 200;

/// Status values a learning event can hold. Stored as text for readability in
/// the database and forward compatibility.
pub mod status {
    /// Recorded, not yet acted on.
    pub const PENDING: &str = "pending";
    /// Promoted into the correction dictionary.
    pub const ACTIVE: &str = "active";
    /// The user said no. Never suggested again.
    pub const DISMISSED: &str = "dismissed";
}

/// A correction the user has made often enough to be worth offering as a rule.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct LearningSuggestion {
    pub id: i64,
    /// What the model produced.
    pub before: String,
    /// What the user changed it to.
    pub after: String,
    /// Stable kind identifier — see [`EditKind::as_str`].
    pub kind: String,
    /// Whether this teaches vocabulary (dictionary) rather than style.
    pub is_vocabulary: bool,
    pub occurrences: i64,
    pub last_seen: i64,
}

pub struct LearningManager {
    db_path: PathBuf,
}

/// Normalise a side of a correction into its dedup key.
///
/// Case-insensitive so "Andy"→"Handy" and "andy"→"handy" are recognised as the
/// same lesson learned twice rather than two lessons learned once — which is the
/// whole point of the frequency gate.
///
/// Edge punctuation is stripped for the same reason, via the same [`core`] the
/// diff uses. Without it, "grandmaster" and "grandmaster," were two different
/// lessons, so a correction the user genuinely made twice sat at one occurrence
/// on each of two rows and was never offered — while
/// [`crate::audio_toolkit::corrections::compile`] had *already* stripped the
/// same punctuation when applying rules. The learner and the matcher disagreeing
/// about what counts as the same word is the bug; this makes them agree.
///
/// Interior punctuation is preserved ("e.g." keeps its inner dot), because that
/// is a different word, not a differently-typed one.
fn key_of(text: &str) -> String {
    text.split_whitespace()
        .map(|w| core(w).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl LearningManager {
    /// Shares `history.db` with [`crate::managers::history`]; the schema is
    /// created by that module's migration list so there is exactly one migration
    /// runner over the file.
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        Ok(Self {
            db_path: app_data_dir.join("history.db"),
        })
    }

    fn connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    /// Re-normalise every stored correction key through the current [`key_of`],
    /// merging rows that collapse onto the same key. Runs at most once per
    /// database, guarded by a marker in `schema_meta`.
    ///
    /// Needed because [`key_of`] used to keep edge punctuation. Rows written
    /// under the old rule are stranded: "grandmaster" and "grandmaster," sit at
    /// one occurrence each, and neither will ever reach the promotion threshold
    /// no matter how many times the user makes that correction. Simply changing
    /// `key_of` fixes new rows and leaves those two there forever, so the
    /// history has to be rewritten too.
    ///
    /// Merging is where the care is. Occurrences **sum** — that is the entire
    /// point, since the counts were only ever split by a normalisation
    /// accident. Status resolves by precedence `active > dismissed > pending`:
    /// a rule already promoted outranks a refusal, and a refusal outranks a
    /// fresh suggestion, so a merge can never resurrect something the user has
    /// already said no to. Re-suggesting a dismissed correction is exactly how
    /// this feature would turn into a nag.
    ///
    /// The unique index is dropped for the rewrite and recreated after. Updating
    /// keys in place would otherwise trip it partway through, when a row being
    /// moved onto its new key meets a row that has not been processed yet.
    pub fn migrate_keys(&self) -> Result<()> {
        let mut conn = self.connection()?;
        Self::migrate_keys_with(&mut conn)
    }

    /// The body of [`Self::migrate_keys`], against a caller-supplied connection
    /// so the merge rules can be tested without a Tauri AppHandle.
    fn migrate_keys_with(conn: &mut Connection) -> Result<()> {
        let already: Option<String> = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'rekey_v1'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if already.is_some() {
            return Ok(());
        }

        let tx = conn.transaction()?;

        struct Row {
            id: i64,
            before_text: String,
            after_text: String,
            first_seen: i64,
            last_seen: i64,
            occurrences: i64,
            status: String,
        }

        let rows: Vec<Row> = {
            let mut stmt = tx.prepare(
                "SELECT id, before_text, after_text, first_seen, last_seen, occurrences, status
                 FROM learning_events ORDER BY id",
            )?;
            let mapped = stmt.query_map([], |row| {
                Ok(Row {
                    id: row.get(0)?,
                    before_text: row.get(1)?,
                    after_text: row.get(2)?,
                    first_seen: row.get(3)?,
                    last_seen: row.get(4)?,
                    occurrences: row.get(5)?,
                    status: row.get(6)?,
                })
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };

        // Precedence, highest wins. Unknown values sort lowest so a row written
        // by a newer build can never silently outrank an explicit dismissal.
        fn rank(s: &str) -> u8 {
            match s {
                status::ACTIVE => 2,
                status::DISMISSED => 1,
                _ => 0,
            }
        }

        // Insertion-ordered so the surviving id is always the lowest in a group,
        // keeping ids stable for every row that does not actually collide.
        let mut groups: Vec<((String, String), Row)> = Vec::new();
        let mut doomed: Vec<i64> = Vec::new();

        for row in rows {
            let key = (key_of(&row.before_text), key_of(&row.after_text));

            // A correction that normalises to nothing, or to a no-op, can never
            // become a rule. It could only ever clutter the suggestion list.
            if key.0.is_empty() || key.1.is_empty() || key.0 == key.1 {
                doomed.push(row.id);
                continue;
            }

            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, keep)) => {
                    keep.occurrences += row.occurrences;
                    keep.first_seen = keep.first_seen.min(row.first_seen);
                    // The most recently seen wording is the one to show the
                    // user, so the surviving display text follows last_seen.
                    if row.last_seen >= keep.last_seen {
                        keep.last_seen = row.last_seen;
                        keep.before_text = row.before_text.clone();
                        keep.after_text = row.after_text.clone();
                    }
                    if rank(&row.status) > rank(&keep.status) {
                        keep.status = row.status.clone();
                    }
                    doomed.push(row.id);
                }
                None => groups.push((key, row)),
            }
        }

        tx.execute("DROP INDEX IF EXISTS idx_learning_events_key", [])?;

        for id in &doomed {
            tx.execute("DELETE FROM learning_events WHERE id = ?1", params![id])?;
        }

        for ((before_key, after_key), row) in &groups {
            tx.execute(
                "UPDATE learning_events
                 SET before_key = ?1, after_key = ?2, before_text = ?3, after_text = ?4,
                     first_seen = ?5, last_seen = ?6, occurrences = ?7, status = ?8
                 WHERE id = ?9",
                params![
                    before_key,
                    after_key,
                    row.before_text,
                    row.after_text,
                    row.first_seen,
                    row.last_seen,
                    row.occurrences,
                    row.status,
                    row.id,
                ],
            )?;
        }

        tx.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_learning_events_key
             ON learning_events (before_key, after_key)",
            [],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('rekey_v1', ?1)",
            params![chrono::Utc::now().timestamp().to_string()],
        )?;

        tx.commit()?;
        debug!(
            "Re-keyed learning events: {} kept, {} merged or dropped",
            groups.len(),
            doomed.len()
        );
        Ok(())
    }

    /// Diff a transcript against the user's edited version and record whatever
    /// can be learned from it.
    ///
    /// Returns the number of events recorded or incremented. Non-promotable
    /// edits (rewrites, insertions, deletions) are skipped entirely rather than
    /// stored, because storing them would only create a pile of rows that can
    /// never become rules and would slow every suggestion query.
    ///
    /// A correction the user has already **dismissed** stays dismissed: its
    /// counter still advances, so the history is honest, but its status is not
    /// reset. Re-suggesting something the user has already said no to is how a
    /// helpful feature turns into a nag.
    pub fn record_edit(
        &self,
        history_id: Option<i64>,
        original: &str,
        edited: &str,
        model_id: Option<&str>,
        language: Option<&str>,
    ) -> Result<usize> {
        let edits: Vec<_> = diff_transcripts(original, edited)
            .into_iter()
            .filter(|e| e.kind.is_promotable())
            .filter(|e| !e.before.trim().is_empty() && !e.after.trim().is_empty())
            .collect();

        if edits.is_empty() {
            return Ok(0);
        }

        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().timestamp();
        let mut recorded = 0usize;

        for edit in &edits {
            let before_key = key_of(&edit.before);
            let after_key = key_of(&edit.after);

            // A correction that normalises to a no-op teaches nothing.
            if before_key == after_key || before_key.is_empty() || after_key.is_empty() {
                continue;
            }

            tx.execute(
                "INSERT INTO learning_events (
                    history_id, first_seen, last_seen, before_text, after_text,
                    before_key, after_key, edit_kind, model_id, language,
                    occurrences, status
                 ) VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)
                 ON CONFLICT (before_key, after_key) DO UPDATE SET
                    occurrences = occurrences + 1,
                    last_seen = excluded.last_seen,
                    history_id = excluded.history_id",
                params![
                    history_id,
                    now,
                    edit.before,
                    edit.after,
                    before_key,
                    after_key,
                    edit.kind.as_str(),
                    model_id,
                    language,
                    status::PENDING,
                ],
            )?;
            recorded += 1;
        }

        tx.commit()?;
        debug!("Learning loop recorded {recorded} correction event(s)");
        Ok(recorded)
    }

    /// Corrections seen at least `min_occurrences` times that are still pending.
    ///
    /// Ordered by how often they have been made, so the most annoying recurring
    /// error is the first thing the user is offered.
    pub fn pending_suggestions(&self, min_occurrences: i64) -> Result<Vec<LearningSuggestion>> {
        // A threshold below 1 would offer every edit ever made, defeating the
        // gate; clamp rather than trusting the caller.
        let min_occurrences = min_occurrences.max(1);
        let conn = self.connection()?;

        let mut stmt = conn.prepare(
            "SELECT id, before_text, after_text, edit_kind, occurrences, last_seen
             FROM learning_events
             WHERE status = ?1 AND occurrences >= ?2
             ORDER BY occurrences DESC, last_seen DESC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            params![status::PENDING, min_occurrences, MAX_SUGGESTIONS],
            |row| {
                let kind: String = row.get(3)?;
                Ok(LearningSuggestion {
                    id: row.get(0)?,
                    before: row.get(1)?,
                    after: row.get(2)?,
                    is_vocabulary: EditKind::from_stored(&kind).is_vocabulary(),
                    kind,
                    occurrences: row.get(4)?,
                    last_seen: row.get(5)?,
                })
            },
        )?;

        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mark a suggestion accepted (`active`) or refused (`dismissed`).
    ///
    /// Rejects any other status rather than writing a value the queries above
    /// would silently never match.
    pub fn set_status(&self, id: i64, new_status: &str) -> Result<()> {
        if !matches!(
            new_status,
            status::PENDING | status::ACTIVE | status::DISMISSED
        ) {
            anyhow::bail!("unknown learning event status: {new_status}");
        }
        self.connection()?.execute(
            "UPDATE learning_events SET status = ?1 WHERE id = ?2",
            params![new_status, id],
        )?;
        Ok(())
    }

    /// Return `active` events whose dictionary entry no longer exists to
    /// `pending`, so they can be suggested again.
    ///
    /// A promoted correction is marked `active` and its pair is written to
    /// `correction_pairs`. But the pair can also be deleted directly from the
    /// dictionary UI, which knows nothing about this table. Without this
    /// reconciliation the event would sit at `active` forever: the rule no
    /// longer exists, yet the correction would never be offered again no matter
    /// how many times the user re-made it. The loop would have silently gone
    /// deaf to one specific mistake.
    ///
    /// Idempotent, and a no-op in the common case.
    pub fn reconcile_active(&self, existing_pairs: &[(String, String)]) -> Result<usize> {
        let active = self.active_rules()?;
        if active.is_empty() {
            return Ok(0);
        }

        let known: Vec<(String, String)> = existing_pairs
            .iter()
            .map(|(wrong, correct)| (wrong.to_lowercase(), correct.to_lowercase()))
            .collect();

        let orphaned: Vec<i64> = active
            .iter()
            .filter(|rule| {
                let key = (rule.before.to_lowercase(), rule.after.to_lowercase());
                !known.contains(&key)
            })
            .map(|rule| rule.id)
            .collect();

        if orphaned.is_empty() {
            return Ok(0);
        }

        let conn = self.connection()?;
        for id in &orphaned {
            conn.execute(
                "UPDATE learning_events SET status = ?1 WHERE id = ?2 AND status = ?3",
                params![status::PENDING, id, status::ACTIVE],
            )?;
        }

        debug!(
            "Returned {} learned correction(s) to pending: their dictionary entries were removed",
            orphaned.len()
        );
        Ok(orphaned.len())
    }

    /// Recent transcript text, newest first, for dictionary mining.
    ///
    /// Reads only the text column: mining needs the user's vocabulary, not their
    /// audio, timings, or anything else the row holds.
    pub fn recent_transcripts(&self, limit: i64) -> Result<Vec<String>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT transcription_text FROM transcription_history
             WHERE TRIM(transcription_text) != ''
             ORDER BY timestamp DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every correction the user has promoted, newest first.
    pub fn active_rules(&self) -> Result<Vec<LearningSuggestion>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, before_text, after_text, edit_kind, occurrences, last_seen
             FROM learning_events
             WHERE status = ?1
             ORDER BY last_seen DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![status::ACTIVE, MAX_SUGGESTIONS], |row| {
            let kind: String = row.get(3)?;
            Ok(LearningSuggestion {
                id: row.get(0)?,
                before: row.get(1)?,
                after: row.get(2)?,
                is_vocabulary: EditKind::from_stored(&kind).is_vocabulary(),
                kind,
                occurrences: row.get(4)?,
                last_seen: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// The learning-table DDL, mirroring the migration in `managers::history`.
    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE learning_events (
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
            CREATE UNIQUE INDEX idx_learning_events_key
                ON learning_events (before_key, after_key);",
        )
        .expect("create learning_events");
        conn
    }

    /// Mirrors `record_edit`'s upsert against a caller-supplied connection, so
    /// the SQL is exercised without needing a Tauri AppHandle.
    fn record(conn: &Connection, original: &str, edited: &str) -> usize {
        let mut recorded = 0;
        for edit in diff_transcripts(original, edited)
            .into_iter()
            .filter(|e| e.kind.is_promotable())
        {
            let bk = key_of(&edit.before);
            let ak = key_of(&edit.after);
            if bk == ak || bk.is_empty() || ak.is_empty() {
                continue;
            }
            conn.execute(
                "INSERT INTO learning_events (
                    history_id, first_seen, last_seen, before_text, after_text,
                    before_key, after_key, edit_kind, model_id, language,
                    occurrences, status
                 ) VALUES (NULL, 1, 1, ?1, ?2, ?3, ?4, ?5, NULL, NULL, 1, 'pending')
                 ON CONFLICT (before_key, after_key) DO UPDATE SET
                    occurrences = occurrences + 1,
                    last_seen = excluded.last_seen",
                params![edit.before, edit.after, bk, ak, edit.kind.as_str()],
            )
            .expect("insert");
            recorded += 1;
        }
        recorded
    }

    fn occurrences(conn: &Connection, before: &str) -> i64 {
        conn.query_row(
            "SELECT occurrences FROM learning_events WHERE before_key = ?1",
            params![key_of(before)],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    #[test]
    fn a_correction_is_recorded_once() {
        let conn = test_db();
        assert_eq!(record(&conn, "i use andy", "i use Handy"), 1);
        assert_eq!(occurrences(&conn, "andy"), 1);
    }

    /// The bug behind the "Grandmaster" report: the same correction made once
    /// mid-sentence and once before a comma used to land on two rows, so it
    /// stayed at one occurrence each and was never offered for promotion.
    #[test]
    fn edge_punctuation_does_not_split_a_correction() {
        assert_eq!(key_of("grandmaster,"), key_of("Grandmaster"));
        assert_eq!(key_of("\"Grant Master\"."), key_of("grant master"));

        let conn = test_db();
        record(&conn, "the grandmaster spoke", "the Grant Master spoke");
        record(&conn, "a grandmaster, yes", "a Grant Master, yes");
        assert_eq!(occurrences(&conn, "grandmaster"), 2);
    }

    /// Interior punctuation is a different word, not a differently-typed one.
    #[test]
    fn interior_punctuation_is_preserved() {
        assert_ne!(key_of("e.g."), key_of("eg"));
    }

    fn seed(conn: &Connection, before: &str, after: &str, occ: i64, status: &str, last: i64) {
        conn.execute(
            "INSERT INTO learning_events (
                history_id, first_seen, last_seen, before_text, after_text,
                before_key, after_key, edit_kind, model_id, language,
                occurrences, status
             ) VALUES (NULL, 1, ?5, ?1, ?2, ?3, ?4, 'substitution', NULL, NULL, ?6, ?7)",
            // Deliberately the *old* key format: lowercased, punctuation kept.
            params![
                before,
                after,
                before.to_lowercase(),
                after.to_lowercase(),
                last,
                occ,
                status
            ],
        )
        .expect("seed");
    }

    fn meta_table(conn: &Connection) {
        conn.execute_batch("CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .expect("schema_meta");
    }

    #[test]
    fn rekey_merges_rows_the_old_format_had_split() {
        let mut conn = test_db();
        meta_table(&conn);
        seed(&conn, "Grandmaster", "Grant Master", 1, status::PENDING, 10);
        seed(
            &conn,
            "grandmaster,",
            "Grant Master,",
            1,
            status::PENDING,
            20,
        );

        LearningManager::migrate_keys_with(&mut conn).expect("rekey");

        let (n, occ): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(occurrences), 0) FROM learning_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "the two rows should have collapsed into one");
        assert_eq!(occ, 2, "counts must sum - that is the whole point");
    }

    /// A merge must never resurrect something the user already refused.
    #[test]
    fn rekey_lets_a_dismissal_outrank_a_pending_row() {
        let mut conn = test_db();
        meta_table(&conn);
        seed(
            &conn,
            "Grandmaster",
            "Grant Master",
            3,
            status::DISMISSED,
            10,
        );
        seed(
            &conn,
            "grandmaster,",
            "Grant Master,",
            1,
            status::PENDING,
            20,
        );

        LearningManager::migrate_keys_with(&mut conn).expect("rekey");

        let status: String = conn
            .query_row("SELECT status FROM learning_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, status::DISMISSED);
    }

    /// ...but a rule already promoted outranks the refusal.
    #[test]
    fn rekey_lets_active_outrank_dismissed() {
        let mut conn = test_db();
        meta_table(&conn);
        seed(
            &conn,
            "Grandmaster",
            "Grant Master",
            1,
            status::DISMISSED,
            10,
        );
        seed(
            &conn,
            "grandmaster,",
            "Grant Master,",
            1,
            status::ACTIVE,
            20,
        );

        LearningManager::migrate_keys_with(&mut conn).expect("rekey");

        let status: String = conn
            .query_row("SELECT status FROM learning_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, status::ACTIVE);
    }

    #[test]
    fn rekey_runs_only_once() {
        let mut conn = test_db();
        meta_table(&conn);
        seed(&conn, "Grandmaster", "Grant Master", 1, status::PENDING, 10);
        seed(
            &conn,
            "grandmaster,",
            "Grant Master,",
            1,
            status::PENDING,
            20,
        );

        LearningManager::migrate_keys_with(&mut conn).expect("first");
        // A second pass must not double-count the already-merged occurrences.
        LearningManager::migrate_keys_with(&mut conn).expect("second");

        let occ: i64 = conn
            .query_row("SELECT SUM(occurrences) FROM learning_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(occ, 2);
    }

    /// A row that normalises to nothing can never become a rule.
    #[test]
    fn rekey_drops_unlearnable_rows() {
        let mut conn = test_db();
        meta_table(&conn);
        seed(&conn, "...", "!!!", 5, status::PENDING, 10);

        LearningManager::migrate_keys_with(&mut conn).expect("rekey");

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn the_same_correction_twice_increments_rather_than_duplicating() {
        let conn = test_db();
        record(&conn, "i use andy", "i use Handy");
        record(&conn, "andy is good", "Handy is good");
        assert_eq!(occurrences(&conn, "andy"), 2);

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the correction should be one row, not two");
    }

    #[test]
    fn case_variants_count_as_the_same_lesson() {
        let conn = test_db();
        record(&conn, "andy is good", "Handy is good");
        record(&conn, "Andy is good", "handy is good");
        assert_eq!(occurrences(&conn, "andy"), 2);
    }

    #[test]
    fn a_rewrite_is_never_recorded() {
        let conn = test_db();
        record(
            &conn,
            "the quick brown fox jumps over the lazy dog",
            "an entirely different sentence written out here instead of that",
        );
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "rewrites must never become learnable events");
    }

    #[test]
    fn insertions_and_deletions_are_not_recorded() {
        let conn = test_db();
        record(&conn, "i use it", "i really use it");
        record(&conn, "i really use it", "i use it");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM learning_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn an_unchanged_transcript_records_nothing() {
        let conn = test_db();
        assert_eq!(record(&conn, "same text", "same text"), 0);
    }

    #[test]
    fn multi_word_corrections_are_recorded_as_one_rule() {
        let conn = test_db();
        record(&conn, "i use andy plus daily", "i use Handy Plus daily");
        let (before, after): (String, String) = conn
            .query_row(
                "SELECT before_text, after_text FROM learning_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(before, "andy plus");
        assert_eq!(after, "Handy Plus");
    }

    #[test]
    fn the_frequency_gate_holds_a_single_occurrence_back() {
        let conn = test_db();
        record(&conn, "i use andy", "i use Handy");

        let below: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_events
                 WHERE status = 'pending' AND occurrences >= ?1",
                params![DEFAULT_PROMOTION_THRESHOLD],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(below, 0, "one occurrence must not be promotable");

        record(&conn, "andy again", "Handy again");
        let at: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_events
                 WHERE status = 'pending' AND occurrences >= ?1",
                params![DEFAULT_PROMOTION_THRESHOLD],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(at, 1, "two occurrences should clear the gate");
    }

    #[test]
    fn a_dismissed_correction_keeps_counting_but_is_not_re_suggested() {
        let conn = test_db();
        record(&conn, "i use andy", "i use Handy");
        conn.execute("UPDATE learning_events SET status = 'dismissed'", [])
            .unwrap();
        record(&conn, "andy again", "Handy again");

        assert_eq!(occurrences(&conn, "andy"), 2);
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM learning_events WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0, "a dismissed correction must stay dismissed");
    }

    #[test]
    fn key_normalisation_collapses_whitespace_and_case() {
        assert_eq!(key_of("  Andy   Plus  "), "andy plus");
        assert_eq!(key_of("ANDY PLUS"), key_of("andy plus"));
    }

    #[test]
    fn unknown_stored_kinds_degrade_to_the_non_promotable_one() {
        // Forward compatibility: a row written by a newer build must not become
        // an active rule just because this build cannot classify it.
        assert_eq!(EditKind::from_stored("something_new"), EditKind::Rewrite);
        assert!(!EditKind::from_stored("something_new").is_promotable());
    }
}
